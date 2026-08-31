//! C ABI entry points for language bindings.
//!
//! The API mirrors the CLI `export-text` and `export-markdown` commands and
//! returns a UTF-8 JSON result string. Call `rhwp_string_free` for every string
//! returned from this module.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};

use rhwp_core::document_core::queries::field_query::{FieldInfo, NestedEntry};
use rhwp_core::model::control::Control;
use rhwp_core::parser::{detect_format, FileFormat};
use rhwp_core::wasm_api::HwpDocument;

mod session;

const ALL_PAGES: i32 = -1;

thread_local! {
    /// 정수를 돌려주는 진입점의 실패 사유. 문자열을 돌려주는 진입점은 JSON 봉투에
    /// 오류를 직접 싣기 때문에 이 값을 쓰지 않는다.
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_last_error(message: impl Into<String>) {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = message.into());
}

fn take_last_error() -> String {
    LAST_ERROR.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

#[no_mangle]
pub extern "C" fn rhwp_export_text(
    input_path: *const c_char,
    output_dir: *const c_char,
    page: i32,
) -> *mut c_char {
    ffi_result(|| {
        let input_path = read_utf8(input_path, "input_path")?;
        let output_dir = read_utf8(output_dir, "output_dir")?;
        export_text_to_dir(
            Path::new(&input_path),
            Path::new(&output_dir),
            normalize_page(page)?,
        )
    })
}

#[no_mangle]
pub extern "C" fn rhwp_export_markdown(
    input_path: *const c_char,
    output_dir: *const c_char,
    page: i32,
) -> *mut c_char {
    ffi_result(|| {
        let input_path = read_utf8(input_path, "input_path")?;
        let output_dir = read_utf8(output_dir, "output_dir")?;
        export_markdown_to_dir(
            Path::new(&input_path),
            Path::new(&output_dir),
            normalize_page(page)?,
        )
    })
}

#[no_mangle]
pub extern "C" fn rhwp_read_text(input_path: *const c_char, page: i32) -> *mut c_char {
    ffi_result(|| {
        let input_path = read_utf8(input_path, "input_path")?;
        read_text(Path::new(&input_path), normalize_page(page)?)
    })
}

/// 이 모듈이 돌려준 문자열을 해제한다.
///
/// # Safety
///
/// `ptr` 은 이 모듈의 `rhwp_export_text`·`rhwp_export_markdown`·`rhwp_read_text` 가
/// 돌려준 포인터이거나 널이어야 한다. 다른 할당자에서 온 포인터를 넘기거나 같은
/// 포인터를 두 번 넘기면 정의되지 않은 동작이다.
///
/// C ABI 심볼은 바뀌지 않으므로 C#·Swift 등 호출 측 선언은 그대로 쓸 수 있다.
#[no_mangle]
pub unsafe extern "C" fn rhwp_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        drop(CString::from_raw(ptr));
    }
}

// ── 문서 세션 표면 ─────────────────────────────────────────────────────────
//
// 위쪽 진입점들은 호출마다 파일을 다시 파싱하고 결과를 파일로 떨군다. 마크다운을
// HWPX 로 조립하는 작업에는 맞지 않는다 — 편집이 호출 사이에 남아야 하기 때문이다.
// 아래 표면은 문서를 세션으로 들고 있으면서 편집하고 마지막에 한 번 저장한다.
//
// 핸들은 포인터가 아니라 정수다(`session` 모듈 참조). 0 은 언제나 실패를 뜻하고,
// 그때의 사유는 `rhwp_last_error()` 로 조회한다.

/// 정수를 돌려주는 진입점용 패닉 가드.
///
/// `ffi_result` 는 문자열 전용이라 여기서는 쓸 수 없다. 가드가 없으면 패닉이 FFI
/// 경계를 넘어 호출 측 프로세스를 즉시 죽인다.
fn guard_u64<F>(f: F) -> u64
where
    F: FnOnce() -> Result<u64, String> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            set_last_error(error);
            0
        }
        Err(_) => {
            set_last_error("FFI 호출 중 panic이 발생했습니다.");
            0
        }
    }
}

/// 마지막 오류를 JSON 으로 돌려준다. 읽으면 비워진다.
///
/// `rhwp_string_free` 로 해제한다.
#[no_mangle]
pub extern "C" fn rhwp_last_error() -> *mut c_char {
    ffi_result(|| {
        let error = take_last_error();
        Ok(format!(
            "{{\"error\":\"{}\"}}",
            json_escape(&error)
        ))
    })
}

/// HWPX 여부를 확인한다.
///
/// C# 쪽이 이미 같은 검사를 하지만(`RhwpFormat`), 네이티브 표면을 직접 부르는
/// 경로가 있으므로 여기서도 막는다. 다만 여기서는 ZIP 매직까지만 본다 — `mimetype`
/// 엔트리 값 대조는 C# 쪽 검사가 맡는다.
fn ensure_hwpx(data: &[u8]) -> Result<(), String> {
    match detect_format(data) {
        FileFormat::Hwpx => Ok(()),
        FileFormat::Hwp | FileFormat::Hwp3 => {
            Err("HWPX 형식만 지원합니다. 입력이 HWP 바이너리입니다.".to_string())
        }
        FileFormat::Hml => Err("HWPX 형식만 지원합니다. 입력이 HWPML(.hml)입니다.".to_string()),
        FileFormat::DrmProtected => {
            Err("DRM 으로 보호된 문서입니다. 보호를 해제한 뒤 저장해 주세요.".to_string())
        }
        FileFormat::Empty => Err("빈 파일(0 바이트)입니다.".to_string()),
        FileFormat::Unknown => Err("HWPX 형식만 지원합니다.".to_string()),
    }
}

/// HWPX 파일을 열고 핸들을 돌려준다. 실패 시 0 — 사유는 `rhwp_last_error()`.
#[no_mangle]
pub extern "C" fn rhwp_document_open(input_path: *const c_char) -> u64 {
    guard_u64(|| {
        let input_path = read_utf8(input_path, "input_path")?;
        let data = fs::read(&input_path)
            .map_err(|e| format!("파일을 읽을 수 없습니다 - {}: {}", input_path, e))?;
        open_from_bytes(&data)
    })
}

/// 바이트에서 직접 연다. 업로드 스트림을 임시 파일로 떨구지 않기 위함이다.
///
/// # Safety
///
/// `data` 는 `len` 바이트를 읽을 수 있는 유효한 포인터여야 한다.
#[no_mangle]
pub unsafe extern "C" fn rhwp_document_open_bytes(data: *const u8, len: usize) -> u64 {
    guard_u64(|| {
        if data.is_null() {
            return Err("data가 null입니다.".to_string());
        }
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        open_from_bytes(bytes)
    })
}

fn open_from_bytes(data: &[u8]) -> Result<u64, String> {
    ensure_hwpx(data)?;
    let document = HwpDocument::from_bytes(data).map_err(|e| format!("HWPX 파싱 실패 - {}", e))?;
    session::insert(document)
}

/// 핸들을 해제한다. 널/이중 해제는 무시한다.
#[no_mangle]
pub extern "C" fn rhwp_document_close(handle: u64) {
    let _ = std::panic::catch_unwind(|| session::remove(handle));
}

/// 열려 있는 문서 수. 누수 점검용이다.
#[no_mangle]
pub extern "C" fn rhwp_document_open_count() -> u64 {
    std::panic::catch_unwind(|| session::count() as u64).unwrap_or(0)
}

/// 세션 문서의 구조 정보. 붙여넣을 위치를 정하는 데 쓴다.
#[no_mangle]
pub extern "C" fn rhwp_document_info(handle: u64) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            let section_count = document.get_section_count() as usize;
            let paragraph_counts: Vec<String> = (0..section_count)
                .map(|section| {
                    document
                        .get_paragraph_count_native(section)
                        .map(|count| count.to_string())
                        .unwrap_or_else(|_| "0".to_string())
                })
                .collect();

            Ok(format!(
                "{{\"ok\":true,\"pageCount\":{},\"sectionCount\":{},\"paragraphCounts\":[{}]}}",
                document.page_count(),
                section_count,
                paragraph_counts.join(",")
            ))
        })
    })
}

/// HTML 조각을 지정한 위치에 붙여넣는다.
///
/// 마크다운을 HWPX 로 만드는 경로의 핵심이다. 호출 측이 마크다운을 HTML 로 바꿔
/// 넘기면 rhwp 가 문단·표·이미지로 변환해 문서에 심는다.
#[no_mangle]
pub extern "C" fn rhwp_document_paste_html(
    handle: u64,
    section: u32,
    paragraph: u32,
    char_offset: u32,
    html: *const c_char,
) -> *mut c_char {
    ffi_result(move || {
        let html = read_utf8(html, "html")?;
        session::with(handle, |document| {
            document
                .paste_html_native(
                    section as usize,
                    paragraph as usize,
                    char_offset as usize,
                    &html,
                )
                .map_err(|e| format!("HTML 붙여넣기 실패 - {}", e))
        })
    })
}

/// HTML 조각을 문서 맨 끝에 붙여넣는다.
///
/// 위치를 직접 계산하지 않아도 되는 흔한 경우를 위한 편의 함수다. 마지막 구역의
/// 마지막 문단 끝에 붙으므로, **그 문단에 내용이 있으면 첫 문단이 이어 붙는다.**
/// 빈 문단으로 끝나는 템플릿에서 자연스럽게 동작한다.
#[no_mangle]
pub extern "C" fn rhwp_document_append_html(handle: u64, html: *const c_char) -> *mut c_char {
    ffi_result(move || {
        let html = read_utf8(html, "html")?;
        session::with(handle, |document| {
            let section_count = document.get_section_count() as usize;
            if section_count == 0 {
                return Err("문서에 구역이 없습니다.".to_string());
            }
            let section = section_count - 1;

            let paragraph_count = document
                .get_paragraph_count_native(section)
                .map_err(|e| format!("문단 수 조회 실패 - {}", e))?;
            if paragraph_count == 0 {
                return Err("마지막 구역에 문단이 없습니다.".to_string());
            }
            let paragraph = paragraph_count - 1;

            let char_offset = document
                .get_paragraph_length_native(section, paragraph)
                .map_err(|e| format!("문단 길이 조회 실패 - {}", e))?;

            document
                .paste_html_native(section, paragraph, char_offset, &html)
                .map_err(|e| format!("HTML 붙여넣기 실패 - {}", e))
        })
    })
}

/// 이름으로 누름틀 값을 채운다. 같은 이름이 여러 개면 `occurrence` 로 고른다.
///
/// CLI `edit fill-fields` 는 첫 칸만 채우지만 여기서는 몇 번째인지 지정할 수 있다.
/// 표 머리글처럼 같은 이름이 여러 칸에 걸린 서식에서 이 차이가 결정적이다.
#[no_mangle]
pub extern "C" fn rhwp_document_set_field(
    handle: u64,
    name: *const c_char,
    occurrence: u32,
    value: *const c_char,
) -> *mut c_char {
    ffi_result(move || {
        let name = read_utf8(name, "name")?;
        let value = read_utf8(value, "value")?;
        session::with(handle, |document| {
            document
                .set_field_value_by_name_at(&name, occurrence as usize, &value)
                .map_err(|e| format!("누름틀 '{}' 설정 실패 - {}", name, e))
        })
    })
}

/// 문단을 서식·누름틀째 복제해 지정 위치에 넣는다.
///
/// 서식 문서로 보고서를 조립하는 경로의 핵심이다. 템플릿은 각 수준을 한 벌씩만
/// 들고 있으므로, 마크다운의 항목이 다섯 개면 그 수준의 문단을 다섯 벌로 늘린 뒤
/// `rhwp_document_set_field` 로 하나씩 채운다. 복제본은 앞머리 글머리표와 글자
/// 모양까지 원본 그대로라 서식의 단일 출처가 템플릿에 남는다.
#[no_mangle]
pub extern "C" fn rhwp_document_duplicate_paragraph(
    handle: u64,
    section: u32,
    source_paragraph: u32,
    dest_paragraph: u32,
    count: u32,
) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            document
                .duplicate_paragraph_native(
                    section as usize,
                    source_paragraph as usize,
                    dest_paragraph as usize,
                    count as usize,
                )
                .map_err(|e| format!("문단 복제 실패 - {}", e))
        })
    })
}

/// 문단 안의 글자 범위를 다른 글자로 바꾼다.
///
/// 서식 문서의 글머리 번호를 고쳐 쓰는 데 쓴다. 공공 서식은 자동 번호 매기기를
/// 쓰지 않고 번호를 문단의 첫 글자로 직접 넣으므로("1. "), 문단을 복제하면 번호도
/// 그대로 복제된다. 복제본마다 이 함수로 번호만 바꿔 준다.
///
/// **넣고 나서 지운다 — 순서가 중요하다.** 먼저 지우면 새 글자가 삽입 위치의
/// 앞 글자에서 모양을 물려받는데, 그 자리가 문단 첫머리이거나 누름틀 표식 옆이면
/// 엉뚱한 모양(예: 누름틀의 파란색)을 뒤집어쓴다. 바꿀 범위의 끝에 먼저 넣으면
/// 그 범위 마지막 글자의 모양을 물려받는다 — 즉 바꾸려던 글자와 같은 모양이다.
#[no_mangle]
pub extern "C" fn rhwp_document_replace_text(
    handle: u64,
    section: u32,
    paragraph: u32,
    char_offset: u32,
    count: u32,
    text: *const c_char,
) -> *mut c_char {
    ffi_result(move || {
        let text = read_utf8(text, "text")?;
        session::with(handle, |document| {
            let section = section as usize;
            let paragraph = paragraph as usize;
            let offset = char_offset as usize;
            let count = count as usize;

            if !text.is_empty() {
                document
                    .insert_text_native(section, paragraph, offset + count, &text)
                    .map_err(|e| format!("글자 삽입 실패 - {}", e))?;
            }

            if count > 0 {
                document
                    .delete_text_native(section, paragraph, offset, count)
                    .map_err(|e| format!("글자 삭제 실패 - {}", e))?;
            }

            Ok(format!(
                "{{\"ok\":true,\"section\":{},\"paragraph\":{},\"removed\":{},\"inserted\":{}}}",
                section,
                paragraph,
                count,
                text.chars().count()
            ))
        })
    })
}

/// 문단을 지운다.
///
/// 조립이 끝난 뒤 템플릿의 견본 문단을 걷어내는 데 쓴다. 견본을 남겨 두면 안내문이
/// 그대로 인쇄되므로, 늘린 뒤 원본을 지우는 것이 한 벌이다.
#[no_mangle]
pub extern "C" fn rhwp_document_delete_paragraph(
    handle: u64,
    section: u32,
    paragraph: u32,
) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            document
                .delete_paragraph_native(section as usize, paragraph as usize)
                .map_err(|e| format!("문단 삭제 실패 - {}", e))
        })
    })
}

/// 누름틀의 이름과 **문단 위치**를 돌려준다. 조립할 자리를 찾는 데 쓴다.
///
/// CLI `fields --json` 과 겹쳐 보이지만 목적이 다르고, 그래서 내용도 다르다.
/// 저쪽은 사람이 문서를 들여다보는 용도라 안내문·현재값까지 싣는다. 이쪽은
/// "이 수준의 문단이 몇 번인가"만 답한다 — 그 답이 `rhwp_document_duplicate_paragraph`
/// 의 인자가 된다.
///
/// `occurrence` 는 같은 이름 안에서의 순번이며 `rhwp_document_set_field` 의 것과 같다.
#[no_mangle]
pub extern "C" fn rhwp_document_field_anchors(handle: u64) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            let mut seen: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();

            let entries: Vec<String> = document
                .collect_all_fields()
                .iter()
                .map(|info| {
                    let name = info.field.field_name().unwrap_or("").to_string();
                    let occurrence = seen.entry(name.clone()).or_insert(0);
                    let index = *occurrence;
                    *occurrence += 1;

                    // 문단의 글자. 채우기 전 견본이라면 이것이 곧 **앞머리**다 —
                    // 빈 누름틀은 글자를 내놓지 않으므로 `"  □ "` 처럼 글머리표만 남는다.
                    // 공공 서식은 번호를 이 글자로 직접 들고 있어서(자동 번호가 아니다),
                    // 복제본의 번호를 고쳐 쓰려면 이 값에서 번호 자리를 찾아야 한다.
                    let paragraph_text = document
                        .document()
                        .sections
                        .get(info.location.section_index)
                        .and_then(|section| section.paragraphs.get(info.location.para_index))
                        .map(|para| para.text.as_str())
                        .unwrap_or("");

                    format!(
                        "{{\"name\":\"{}\",\"occurrence\":{},\"section\":{},\"paragraph\":{},\"nested\":{},\"text\":\"{}\"{}}}",
                        json_escape(&name),
                        index,
                        info.location.section_index,
                        info.location.para_index,
                        info.location.nested_path.len(),
                        json_escape(paragraph_text),
                        table_cell_json(document, info)
                    )
                })
                .collect();

            Ok(format!(
                "{{\"ok\":true,\"anchors\":[{}]}}",
                entries.join(",")
            ))
        })
    })
}

/// 표 셀에 든 누름틀이면 그 칸의 표 좌표를, 아니면 빈 문자열을 돌려준다.
///
/// 표 조립은 "몇 번째 행"으로 말한다(`rhwp_document_duplicate_table_row`). 그런데
/// `FieldInfo` 가 주는 것은 `cell_index` — `Table::cells` 안에서의 순번이라 행·열이
/// 아니다. 병합이 없는 표에서는 `row * col_count + col` 과 같지만 그 가정은 병합
/// 하나로 깨진다. **추측하지 않고 표에서 직접 읽는다.**
///
/// 깊이 1 의 표 셀만 다룬다. 중첩 표나 글상자 안의 누름틀은 조립 대상이 아니므로
/// 좌표를 싣지 않고, 받는 쪽은 `row` 가 없는 것으로 그것을 안다.
fn table_cell_json(document: &HwpDocument, info: &FieldInfo) -> String {
    let [NestedEntry::TableCell {
        control_index,
        cell_index,
        ..
    }] = info.location.nested_path.as_slice()
    else {
        return String::new();
    };

    let Some(Control::Table(table)) = document
        .document()
        .sections
        .get(info.location.section_index)
        .and_then(|section| section.paragraphs.get(info.location.para_index))
        .and_then(|para| para.controls.get(*control_index))
    else {
        return String::new();
    };

    let Some(cell) = table.cells.get(*cell_index) else {
        return String::new();
    };

    format!(
        ",\"control\":{},\"cell\":{},\"row\":{},\"col\":{},\"rowCount\":{},\"colCount\":{}",
        control_index, cell_index, cell.row, cell.col, table.row_count, table.col_count
    )
}

/// 표의 한 행을 내용·서식·누름틀째 복제해 바로 아래에 넣는다.
///
/// 문단 복제(`rhwp_document_duplicate_paragraph`)의 표 판이다. 자료 행의 칸마다
/// 누름틀이 하나씩 박혀 있고 그 이름이 곧 "이 칸에 무엇을 넣는가"이므로, 복제본도
/// 같은 이름을 들고 있어야 `rhwp_document_set_field` 의 `occurrence` 로 행을 고를 수
/// 있다. 상류 `insert_table_row_native` 는 서식만 물려주고 글자를 비우므로 —
/// 편집기용으로는 그것이 맞다 — 조립 경로에서는 쓸 수 없다.
///
/// 원본 행에 병합된 칸이 있으면 실패한다. 자세한 사유는
/// `DocumentCore::duplicate_table_row_native` 의 주석에 있다.
#[no_mangle]
pub extern "C" fn rhwp_document_duplicate_table_row(
    handle: u64,
    section: u32,
    paragraph: u32,
    control: u32,
    row: u32,
    count: u32,
) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            let row = u16::try_from(row).map_err(|_| format!("행 인덱스 {} 범위 초과", row))?;
            document
                .duplicate_table_row_native(
                    section as usize,
                    paragraph as usize,
                    control as usize,
                    row,
                    count as usize,
                )
                .map_err(|e| format!("표 행 복제 실패 - {}", e))
        })
    })
}

/// 문서의 서식 구조를 한 봉투의 JSON 으로 돌려준다. 서식 템플릿 분석의 입력이다.
///
/// 담기는 것은 **읽은 값뿐이고 판단은 없다.** 어느 문단이 몇 수준인지, 어느 표가
/// 견본인지는 호출 측이 정한다 — 그 규칙은 서식마다 다르고 자주 바뀌므로 네이티브
/// 산출물에 굳혀 두면 규칙 하나 고치는 데 양 플랫폼 재빌드가 든다.
///
/// 조회 함수를 열여덟 개 따로 내보내지 않고 하나로 모은 이유는 봉투 모양 때문이다.
/// 상류의 조회들은 같은 개념에 서로 다른 키 이름을 쓴다(5.5.3·8.3 에서 두 번 겪었다).
/// 호출 측이 열여덟 벌의 모델을 들고 키를 맞추다 한 번 어긋나면 **예외 없이 빈 값**을
/// 얻는다. 한 번 훑어 한 봉투면 모델도 하나다.
///
/// 담기는 것:
/// - `info` — 판·구역 수·쪽 수·쓰인 글꼴
/// - `styles` · `numbering` — 스타일 목록과 개요 번호 형식
/// - `sections[].paragraphs[]` — 글자, 스타일, 글자 모양, 문단 모양
/// - `sections[].tables[]` — 표 크기, 표 속성, 칸마다의 자리·속성·글자 모양
#[no_mangle]
pub extern "C" fn rhwp_document_structure(handle: u64) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            document
                .structure_dump_native()
                .map_err(|e| format!("문서 구조 덤프 실패 - {}", e))
        })
    })
}

/// 표 하나를 읽기 전용으로 집는다. 없으면 `None`.
fn table_at<'a>(
    document: &'a HwpDocument,
    section: usize,
    paragraph: usize,
    control: usize,
) -> Option<&'a rhwp_core::model::table::Table> {
    match document
        .document()
        .sections
        .get(section)
        .and_then(|section| section.paragraphs.get(paragraph))
        .and_then(|para| para.controls.get(control))
    {
        Some(Control::Table(table)) => Some(table),
        _ => None,
    }
}

/// 표 칸의 글자를 통째로 바꾼다. 칸은 **행·열**로 지목한다.
///
/// 조립기가 표를 채우는 유일한 통로다. 자리표시자를 누름틀이 아니라 글자로 두는
/// 서식에서는 `rhwp_document_set_field` 를 쓸 수 없고, 칸이 아예 비어 있는 서식도
/// 흔하다 — 그때는 바꿀 누름틀조차 없다.
///
/// **행·열로 받고 칸 순번은 안에서 구한다.** `Table::cells` 의 순번은 행·열이 아니며
/// `row * col_count + col` 이라는 가정은 병합 하나로 깨진다. 호출 측이 그 계산을
/// 하게 두면 병합된 서식에서 조용히 엉뚱한 칸을 채운다.
///
/// **넣고 나서 지운다** — `rhwp_document_replace_text` 와 같은 이유다. 먼저 지우면
/// 새 글자가 빈 문단의 기본 모양을 뒤집어써 서식이 템플릿에서 오지 않는다.
/// 칸의 첫 문단만 다룬다. 서식의 표 칸이 여러 문단을 쓰는 경우는 없었고, 있다면
/// 그것은 자료 칸이 아니다.
#[no_mangle]
pub extern "C" fn rhwp_document_set_cell_text(
    handle: u64,
    section: u32,
    paragraph: u32,
    control: u32,
    row: u32,
    col: u32,
    text: *const c_char,
) -> *mut c_char {
    ffi_result(move || {
        let text = read_utf8(text, "text")?;
        session::with(handle, |document| {
            let section = section as usize;
            let paragraph = paragraph as usize;
            let control = control as usize;

            let row = u16::try_from(row).map_err(|_| format!("행 인덱스 {} 범위 초과", row))?;
            let col = u16::try_from(col).map_err(|_| format!("열 인덱스 {} 범위 초과", col))?;

            let cell = table_at(document, section, paragraph, control)
                .ok_or_else(|| {
                    format!(
                        "표를 찾지 못했습니다 - 구역 {} 문단 {} 컨트롤 {}",
                        section, paragraph, control
                    )
                })?
                .cell_index_at(row, col)
                .ok_or_else(|| format!("표에 {}행 {}열 칸이 없습니다", row, col))?;

            let old = document
                .get_cell_paragraph_length_native(section, paragraph, control, cell, 0)
                .map_err(|e| format!("칸 글자 길이 조회 실패 - {}", e))?;

            if !text.is_empty() {
                document
                    .insert_text_in_cell_native(section, paragraph, control, cell, 0, old, &text)
                    .map_err(|e| format!("칸 글자 삽입 실패 - {}", e))?;
            }

            if old > 0 {
                document
                    .delete_text_in_cell_native(section, paragraph, control, cell, 0, 0, old)
                    .map_err(|e| format!("칸 글자 삭제 실패 - {}", e))?;
            }

            Ok(format!(
                "{{\"ok\":true,\"cell\":{},\"row\":{},\"col\":{},\"removed\":{},\"inserted\":{}}}",
                cell,
                row,
                col,
                old,
                text.chars().count()
            ))
        })
    })
}

/// 표에서 행 하나를 지운다.
///
/// 서식의 표는 자료 행을 몇 벌 갖춰 두는데 이번 문서가 그보다 적게 쓰면 빈 행이
/// 남는다. 늘리는 쪽(`rhwp_document_duplicate_table_row`)의 짝이다.
#[no_mangle]
pub extern "C" fn rhwp_document_delete_table_row(
    handle: u64,
    section: u32,
    paragraph: u32,
    control: u32,
    row: u32,
) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            let row = u16::try_from(row).map_err(|_| format!("행 인덱스 {} 범위 초과", row))?;
            document
                .delete_table_row_native(
                    section as usize,
                    paragraph as usize,
                    control as usize,
                    row,
                )
                .map_err(|e| format!("표 행 삭제 실패 - {}", e))
        })
    })
}

/// 표에 열 하나를 끼운다. `right` 가 참이면 `col` 의 오른쪽이다.
///
/// 새 열은 이웃 열의 서식과 폭을 물려받으므로 **표 전체가 그만큼 넓어진다.**
/// 폭을 되돌리려면 `rhwp_document_set_table_column_widths` 로 다시 나눈다.
#[no_mangle]
pub extern "C" fn rhwp_document_insert_table_column(
    handle: u64,
    section: u32,
    paragraph: u32,
    control: u32,
    col: u32,
    right: bool,
) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            let col = u16::try_from(col).map_err(|_| format!("열 인덱스 {} 범위 초과", col))?;
            document
                .insert_table_column_native(
                    section as usize,
                    paragraph as usize,
                    control as usize,
                    col,
                    right,
                )
                .map_err(|e| format!("표 열 삽입 실패 - {}", e))
        })
    })
}

/// 표에서 열 하나를 지운다.
#[no_mangle]
pub extern "C" fn rhwp_document_delete_table_column(
    handle: u64,
    section: u32,
    paragraph: u32,
    control: u32,
    col: u32,
) -> *mut c_char {
    ffi_result(move || {
        session::with(handle, |document| {
            let col = u16::try_from(col).map_err(|_| format!("열 인덱스 {} 범위 초과", col))?;
            document
                .delete_table_column_native(
                    section as usize,
                    paragraph as usize,
                    control as usize,
                    col,
                )
                .map_err(|e| format!("표 열 삭제 실패 - {}", e))
        })
    })
}

/// 표의 열 폭을 다시 나눈다. `widths` 는 쉼표로 이은 HWPUNIT 값이다.
///
/// 배열 대신 문자열로 받는다. 이 C ABI 는 문자열 하나를 넣고 JSON 하나를 받는
/// 모양으로 통일되어 있고, 열 몇 개를 위해 포인터·길이 쌍을 들이면 그 규약이
/// 깨진다.
#[no_mangle]
pub extern "C" fn rhwp_document_set_table_column_widths(
    handle: u64,
    section: u32,
    paragraph: u32,
    control: u32,
    widths: *const c_char,
) -> *mut c_char {
    ffi_result(move || {
        let widths = read_utf8(widths, "widths")?;
        let parsed: Result<Vec<u32>, _> = widths
            .split(',')
            .map(|item| item.trim().parse::<u32>())
            .collect();
        let parsed = parsed.map_err(|e| format!("열 폭 목록을 읽지 못했습니다 - {}", e))?;

        session::with(handle, |document| {
            document
                .set_table_column_widths_native(
                    section as usize,
                    paragraph as usize,
                    control as usize,
                    parsed.clone(),
                )
                .map_err(|e| format!("표 열 폭 설정 실패 - {}", e))
        })
    })
}

/// 세션 문서를 HWPX 로 저장한다.
#[no_mangle]
pub extern "C" fn rhwp_document_save_hwpx(handle: u64, output_path: *const c_char) -> *mut c_char {
    ffi_result(move || {
        let output_path = read_utf8(output_path, "output_path")?;
        session::with(handle, |document| {
            let bytes = document
                .export_hwpx_native()
                .map_err(|e| format!("HWPX 직렬화 실패 - {}", e))?;

            let path = Path::new(&output_path);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!("출력 폴더를 생성할 수 없습니다 - {}: {}", parent.display(), e)
                    })?;
                }
            }

            let byte_count = bytes.len();
            fs::write(path, &bytes)
                .map_err(|e| format!("HWPX 저장 실패 - {}: {}", output_path, e))?;

            Ok(format!(
                "{{\"ok\":true,\"output\":\"{}\",\"bytes\":{}}}",
                json_escape(&output_path),
                byte_count
            ))
        })
    })
}

fn export_text_to_dir(
    input_path: &Path,
    output_dir: &Path,
    target_page: Option<u32>,
) -> Result<String, String> {
    let data = fs::read(input_path)
        .map_err(|e| format!("파일을 읽을 수 없습니다 - {}: {}", input_path.display(), e))?;
    let doc = HwpDocument::from_bytes(&data).map_err(|e| format!("HWP 파싱 실패 - {}", e))?;
    let page_count = doc.page_count();
    let pages = select_pages(page_count, target_page)?;
    fs::create_dir_all(output_dir).map_err(|e| {
        format!(
            "출력 폴더를 생성할 수 없습니다 - {}: {}",
            output_dir.display(),
            e
        )
    })?;

    let file_stem = file_stem(input_path);
    let mut written = Vec::new();

    for page_num in pages {
        let mut text = doc
            .extract_page_text_native(page_num)
            .map_err(|e| format!("페이지 {} 텍스트 추출 실패 - {:?}", page_num, e))?;
        ensure_trailing_newline(&mut text);

        let output_path = output_dir.join(page_file_name(&file_stem, "txt", page_count, page_num));
        fs::write(&output_path, text.as_bytes())
            .map_err(|e| format!("TXT 저장 실패 - {}: {}", output_path.display(), e))?;
        written.push(output_path);
    }

    Ok(success_json(page_count, &written, None))
}

fn read_text(input_path: &Path, target_page: Option<u32>) -> Result<String, String> {
    let data = fs::read(input_path)
        .map_err(|e| format!("파일을 읽을 수 없습니다 - {}: {}", input_path.display(), e))?;
    let doc = HwpDocument::from_bytes(&data).map_err(|e| format!("HWP 파싱 실패 - {}", e))?;
    let page_count = doc.page_count();
    let pages = select_pages(page_count, target_page)?;

    let mut extracted = Vec::new();
    for page_num in pages {
        let mut text = doc
            .extract_page_text_native(page_num)
            .map_err(|e| format!("페이지 {} 텍스트 추출 실패 - {:?}", page_num, e))?;
        ensure_trailing_newline(&mut text);
        extracted.push((page_num, text));
    }

    Ok(text_json(page_count, &extracted))
}

fn export_markdown_to_dir(
    input_path: &Path,
    output_dir: &Path,
    target_page: Option<u32>,
) -> Result<String, String> {
    let data = fs::read(input_path)
        .map_err(|e| format!("파일을 읽을 수 없습니다 - {}: {}", input_path.display(), e))?;
    let doc = HwpDocument::from_bytes(&data).map_err(|e| format!("HWP 파싱 실패 - {}", e))?;
    let page_count = doc.page_count();
    let pages = select_pages(page_count, target_page)?;
    fs::create_dir_all(output_dir).map_err(|e| {
        format!(
            "출력 폴더를 생성할 수 없습니다 - {}: {}",
            output_dir.display(),
            e
        )
    })?;

    let file_stem = file_stem(input_path);
    let assets_dir_name = format!("{}_assets", file_stem);
    let assets_dir_path = output_dir.join(&assets_dir_name);
    let mut written = Vec::new();
    let mut written_image_count = 0usize;

    for page_num in pages {
        let (mut markdown, image_refs) = doc
            .extract_page_markdown_with_images_native(page_num)
            .map_err(|e| format!("페이지 {} Markdown 생성 실패 - {:?}", page_num, e))?;

        for (img_idx, (sec_idx, para_idx, control_idx, bin_data_id)) in
            image_refs.iter().enumerate()
        {
            let token = format!("[[RHWP_IMAGE:{}]]", img_idx + 1);
            let Some((mime, image_data)) =
                extract_image_data(&doc, *sec_idx, *para_idx, *control_idx, *bin_data_id)?
            else {
                markdown = markdown.replace(&token, "");
                continue;
            };

            fs::create_dir_all(&assets_dir_path).map_err(|e| {
                format!(
                    "이미지 출력 폴더 생성 실패 - {}: {}",
                    assets_dir_path.display(),
                    e
                )
            })?;

            let image_filename = format!(
                "{}_p{:03}_img{:03}.{}",
                file_stem,
                page_num + 1,
                img_idx + 1,
                mime_to_ext(&mime),
            );
            let image_path = assets_dir_path.join(&image_filename);
            fs::write(&image_path, &image_data)
                .map_err(|e| format!("이미지 저장 실패 - {}: {}", image_path.display(), e))?;

            let image_link = format!(
                "![image {}]({}/{})",
                img_idx + 1,
                assets_dir_name,
                image_filename
            );
            markdown = markdown.replace(&token, &image_link);
            written_image_count += 1;
        }

        ensure_trailing_newline(&mut markdown);
        let output_path = output_dir.join(page_file_name(&file_stem, "md", page_count, page_num));
        fs::write(&output_path, markdown.as_bytes())
            .map_err(|e| format!("Markdown 저장 실패 - {}: {}", output_path.display(), e))?;
        written.push(output_path);
    }

    Ok(success_json(
        page_count,
        &written,
        Some(written_image_count),
    ))
}

fn extract_image_data(
    doc: &HwpDocument,
    sec_idx: Option<usize>,
    para_idx: Option<usize>,
    control_idx: Option<usize>,
    bin_data_id: u16,
) -> Result<Option<(String, Vec<u8>)>, String> {
    if let (Some(si), Some(pi), Some(ci)) = (sec_idx, para_idx, control_idx) {
        // [#1161] cell_path 가 비면 본문 문단. FFI 표면은 셀/글상자 안 컨트롤을
        // 아직 지정할 수 없으므로 본문으로 고정한다 — `src/main.rs` 의 CLI 경로와 같다.
        const BODY_PARA: &[(usize, usize, usize)] = &[];
        if let (Ok(mime), Ok(data)) = (
            doc.get_control_image_mime_native(si, pi, BODY_PARA, ci),
            doc.get_control_image_data_native(si, pi, BODY_PARA, ci),
        ) {
            return Ok(Some((mime, data)));
        }
    }

    if bin_data_id == 0 {
        return Ok(None);
    }

    let mime = doc
        .get_bin_data_image_mime_native(bin_data_id)
        .map_err(|e| format!("이미지 MIME fallback 실패 (bin={}): {:?}", bin_data_id, e))?;
    let data = doc
        .get_bin_data_image_data_native(bin_data_id)
        .map_err(|e| format!("이미지 데이터 fallback 실패 (bin={}): {:?}", bin_data_id, e))?;
    Ok(Some((mime, data)))
}

fn select_pages(page_count: u32, target_page: Option<u32>) -> Result<Vec<u32>, String> {
    if page_count == 0 {
        return Err("문서에 페이지가 없습니다.".to_string());
    }

    match target_page {
        Some(page) if page >= page_count => Err(format!(
            "페이지 번호가 범위를 벗어났습니다 (0~{})",
            page_count - 1
        )),
        Some(page) => Ok(vec![page]),
        None => Ok((0..page_count).collect()),
    }
}

fn normalize_page(page: i32) -> Result<Option<u32>, String> {
    if page == ALL_PAGES {
        Ok(None)
    } else if page < 0 {
        Err("page는 -1(전체) 또는 0 이상의 페이지 번호여야 합니다.".to_string())
    } else {
        Ok(Some(page as u32))
    }
}

fn read_utf8(ptr: *const c_char, name: &str) -> Result<String, String> {
    if ptr.is_null() {
        return Err(format!("{}가 null입니다.", name));
    }

    unsafe {
        CStr::from_ptr(ptr)
            .to_str()
            .map(|s| s.to_string())
            .map_err(|e| format!("{}는 유효한 UTF-8 문자열이어야 합니다: {}", name, e))
    }
}

fn ffi_result<F>(f: F) -> *mut c_char
where
    F: FnOnce() -> Result<String, String> + std::panic::UnwindSafe,
{
    let json = match std::panic::catch_unwind(f) {
        Ok(Ok(json)) => json,
        Ok(Err(error)) => error_json(&error),
        Err(_) => error_json("FFI 호출 중 panic이 발생했습니다."),
    };

    CString::new(json)
        .unwrap_or_else(|_| {
            CString::new(error_json("결과 문자열에 NUL 문자가 포함되었습니다.")).unwrap()
        })
        .into_raw()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("page")
        .to_string()
}

fn page_file_name(file_stem: &str, ext: &str, page_count: u32, page_num: u32) -> String {
    if page_count == 1 {
        format!("{}.{}", file_stem, ext)
    } else {
        format!("{}_{:03}.{}", file_stem, page_num + 1, ext)
    }
}

fn ensure_trailing_newline(s: &mut String) {
    if !s.ends_with('\n') {
        s.push('\n');
    }
}

fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        _ => "bin",
    }
}

fn success_json(page_count: u32, files: &[PathBuf], image_count: Option<usize>) -> String {
    let files_json = files
        .iter()
        .map(|p| format!("\"{}\"", json_escape(&p.display().to_string())))
        .collect::<Vec<_>>()
        .join(",");
    let image_json = image_count
        .map(|count| format!(",\"imageCount\":{}", count))
        .unwrap_or_default();

    format!(
        "{{\"ok\":true,\"pageCount\":{},\"files\":[{}]{}}}",
        page_count, files_json, image_json
    )
}

fn error_json(error: &str) -> String {
    format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(error))
}

fn text_json(page_count: u32, pages: &[(u32, String)]) -> String {
    let pages_json = pages
        .iter()
        .map(|(index, text)| format!("{{\"index\":{},\"text\":\"{}\"}}", index, json_escape(text)))
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "{{\"ok\":true,\"pageCount\":{},\"pages\":[{}]}}",
        page_count, pages_json
    )
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\u{20}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
