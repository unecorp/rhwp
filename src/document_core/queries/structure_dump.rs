//! 문서 구조 덤프 — 서식 템플릿을 분석하기 위한 읽기 전용 질의.
//!
//! 템플릿 분석기는 문단마다 글자 모양·문단 모양·스타일을, 표마다 셀 속성을 읽어야
//! 한다. 그 값들은 이미 상류에 다 있으나 **조회 함수 열여덟 개에 흩어져 있고 봉투
//! 모양이 제각각이다.** 그것을 그대로 C ABI 로 내보내면 호출 측이 열여덟 벌의 모델을
//! 들고 키 이름을 맞춰야 하고, 한 번 어긋나면 예외 없이 빈 값을 얻는다 — 이미 두 번
//! 겪은 함정이다.
//!
//! 그래서 **한 번 훑어 한 봉투로** 돌려준다. 호출은 한 번, 모델은 하나다.

use crate::document_core::DocumentCore;
use crate::error::HwpError;
use crate::model::control::Control;

use super::super::helpers::json_escape;

impl DocumentCore {
    /// 문서의 서식 구조를 한 봉투의 JSON 으로 돌려준다.
    ///
    /// 담기는 것은 **읽은 값뿐이고 판단은 없다.** 어느 문단이 몇 수준인지, 어느 표가
    /// 견본인지는 호출 측이 정한다 — 그 규칙은 서식마다 다르고 자주 바뀌므로 네이티브
    /// 산출물에 굳혀 두면 규칙 하나 고치는 데 양 플랫폼 재빌드가 든다.
    ///
    /// 값은 상류 조회 함수가 만든 JSON 을 그대로 싣는다. 다시 풀어 쓰면 글꼴 언어별
    /// 해석 같은 규칙이 두 벌이 되고, 두 벌은 반드시 갈라진다.
    pub fn structure_dump_native(&self) -> Result<String, HwpError> {
        let mut sections = Vec::new();

        for (section_idx, section) in self.document.sections.iter().enumerate() {
            let mut paragraphs = Vec::new();
            let mut tables = Vec::new();

            for (para_idx, para) in section.paragraphs.iter().enumerate() {
                let style_id = para.style_id as usize;
                let style_name = self
                    .document
                    .doc_info
                    .styles
                    .get(style_id)
                    .map(|style| style.local_name.as_str())
                    .unwrap_or("");

                let char_shape = self
                    .get_char_properties_at_native(section_idx, para_idx, 0)
                    .unwrap_or_else(|_| "null".to_string());
                let para_shape = self
                    .get_para_properties_at_native(section_idx, para_idx)
                    .unwrap_or_else(|_| "null".to_string());

                paragraphs.push(format!(
                    "{{\"index\":{},\"text\":\"{}\",\"styleId\":{},\"styleName\":\"{}\",\"charShape\":{},\"paraShape\":{}}}",
                    para_idx,
                    json_escape(&para.text),
                    style_id,
                    json_escape(style_name),
                    char_shape,
                    para_shape
                ));

                for (control_idx, control) in para.controls.iter().enumerate() {
                    let Control::Table(table) = control else {
                        continue;
                    };

                    tables.push(self.table_dump(section_idx, para_idx, control_idx, table));
                }
            }

            sections.push(format!(
                "{{\"index\":{},\"paragraphs\":[{}],\"tables\":[{}]}}",
                section_idx,
                paragraphs.join(","),
                tables.join(",")
            ));
        }

        Ok(format!(
            "{{\"ok\":true,\"info\":{},\"styles\":{},\"numbering\":{},\"sections\":[{}]}}",
            self.get_document_info(),
            self.styles_json(),
            self.numbering_json(),
            sections.join(",")
        ))
    }

    /// 표 하나를 덤프한다. 셀은 표가 정렬해 둔 순서(행 우선) 그대로다.
    fn table_dump(
        &self,
        section_idx: usize,
        para_idx: usize,
        control_idx: usize,
        table: &crate::model::table::Table,
    ) -> String {
        let properties = self
            .get_table_properties_native(section_idx, para_idx, control_idx)
            .unwrap_or_else(|_| "null".to_string());

        let cells: Vec<String> = table
            .cells
            .iter()
            .enumerate()
            .map(|(cell_idx, cell)| {
                let properties = self
                    .get_cell_properties_native(section_idx, para_idx, control_idx, cell_idx)
                    .unwrap_or_else(|_| "null".to_string());
                let char_shape = self
                    .get_cell_char_properties_at_native(
                        section_idx,
                        para_idx,
                        control_idx,
                        cell_idx,
                        0,
                        0,
                    )
                    .unwrap_or_else(|_| "null".to_string());
                let para_shape = self
                    .get_cell_para_properties_at_native(
                        section_idx,
                        para_idx,
                        control_idx,
                        cell_idx,
                        0,
                    )
                    .unwrap_or_else(|_| "null".to_string());

                // 셀의 글자. 첫 문단만 본다 — 서식 분석에 필요한 것은 "이 칸이 무엇을
                // 담는 칸인가"이고, 그 답은 첫 줄에 있다.
                let text = cell
                    .paragraphs
                    .first()
                    .map(|para| para.text.as_str())
                    .unwrap_or("");

                format!(
                    "{{\"index\":{},\"row\":{},\"col\":{},\"rowSpan\":{},\"colSpan\":{},\"width\":{},\"height\":{},\"isHeader\":{},\"text\":\"{}\",\"properties\":{},\"charShape\":{},\"paraShape\":{}}}",
                    cell_idx,
                    cell.row,
                    cell.col,
                    cell.row_span,
                    cell.col_span,
                    cell.width,
                    cell.height,
                    cell.is_header,
                    json_escape(text),
                    properties,
                    char_shape,
                    para_shape
                )
            })
            .collect();

        format!(
            "{{\"section\":{},\"paragraph\":{},\"control\":{},\"rowCount\":{},\"colCount\":{},\"properties\":{},\"cells\":[{}]}}",
            section_idx,
            para_idx,
            control_idx,
            table.row_count,
            table.col_count,
            properties,
            cells.join(",")
        )
    }

    /// 스타일 목록. `HwpDocument::get_style_list` 과 같은 모양이다.
    fn styles_json(&self) -> String {
        let items: Vec<String> = self
            .document
            .doc_info
            .styles
            .iter()
            .enumerate()
            .map(|(id, style)| {
                format!(
                    "{{\"id\":{},\"name\":\"{}\",\"englishName\":\"{}\",\"type\":{},\"nextStyleId\":{},\"paraShapeId\":{},\"charShapeId\":{}}}",
                    id,
                    json_escape(&style.local_name),
                    json_escape(&style.english_name),
                    style.style_type,
                    style.next_style_id,
                    style.para_shape_id,
                    style.char_shape_id
                )
            })
            .collect();

        format!("[{}]", items.join(","))
    }

    /// 개요 번호 목록. `HwpDocument::get_numbering_list` 과 같은 모양이다.
    fn numbering_json(&self) -> String {
        let items: Vec<String> = self
            .document
            .doc_info
            .numberings
            .iter()
            .enumerate()
            .map(|(index, numbering)| {
                let formats: Vec<String> = numbering
                    .level_formats
                    .iter()
                    .map(|format| format!("\"{}\"", json_escape(format)))
                    .collect();

                format!(
                    "{{\"id\":{},\"levelFormats\":[{}],\"startNumber\":{}}}",
                    index + 1,
                    formats.join(","),
                    numbering.start_number
                )
            })
            .collect();

        format!("[{}]", items.join(","))
    }
}
