//! 표 행 복제 — 서식 문서의 표를 자료 행 수만큼 늘리는 편집 연산.

use crate::document_core::DocumentCore;
use crate::error::HwpError;
use crate::model::event::DocumentEvent;
use crate::model::table::Cell;

impl DocumentCore {
    /// 표의 한 행을 내용·서식·누름틀째 복제해 바로 아래에 넣는다.
    ///
    /// [`DocumentCore::insert_table_row_native`] 는 `Cell::new_from_template` 으로 새 칸을
    /// 만든다. 그 함수는 이름대로 **서식만** 물려주고 글자는 비운다 — 새 행에 안내문이
    /// 딸려 오면 곤란하니 편집기용으로는 그것이 맞다. 그러나 서식 문서로 보고서를
    /// 조립하는 경로에서는 반대가 필요하다. 자료 행의 칸마다 누름틀이 하나씩 박혀
    /// 있고, 그 누름틀이 곧 "이 칸에 무엇을 넣는가"의 이름이기 때문이다. 빈 칸으로
    /// 늘리면 이름이 사라져 [`DocumentCore::set_field_value_by_name_at`] 로 채울 수 없다.
    ///
    /// 그래서 문단 복제([`DocumentCore::duplicate_paragraph_native`])와 같은 규약을 표에
    /// 적용한다. 복제본의 누름틀은 원본과 같은 이름을 가지므로 `occurrence` 로 몇 번째
    /// 행인지 고른다. 본문이든 표든 채우는 쪽 코드가 하나로 유지된다.
    ///
    /// # 병합된 행은 거절한다
    ///
    /// `Table::insert_row` 는 병합을 풀어 열마다 한 칸씩 만든다. 원본 행에 `col_span`
    /// 이 2 인 칸이 있으면 복제본은 칸 수부터 달라지고, 그 상태로 내용을 옮기면 표가
    /// **조용히 어긋난 채** 저장된다. 자료 행이 병합을 쓰지 않는 것은 서식 작성 규약에
    /// 속하므로 여기서 실패로 알린다.
    ///
    /// # 인자
    ///
    /// * `section_idx` — 구역 인덱스.
    /// * `parent_para_idx` — 표를 담은 본문 문단.
    /// * `control_idx` — 그 문단 안에서 표가 몇 번째 컨트롤인지.
    /// * `row_idx` — 복제할 원본 행.
    /// * `count` — 복제 벌 수. 0 이면 아무것도 하지 않는다.
    pub fn duplicate_table_row_native(
        &mut self,
        section_idx: usize,
        parent_para_idx: usize,
        control_idx: usize,
        row_idx: u16,
        count: usize,
    ) -> Result<String, HwpError> {
        let table = self.get_table_mut(section_idx, parent_para_idx, control_idx)?;

        if row_idx >= table.row_count {
            return Err(HwpError::RenderError(format!(
                "행 인덱스 {} 범위 초과 (총 {}행)",
                row_idx, table.row_count
            )));
        }

        let source: Vec<Cell> = table
            .cells
            .iter()
            .filter(|cell| cell.row == row_idx)
            .cloned()
            .collect();

        if let Some(merged) = source
            .iter()
            .find(|cell| cell.col_span != 1 || cell.row_span != 1)
        {
            return Err(HwpError::RenderError(format!(
                "행 {} 의 셀(열 {})이 병합되어 있어 복제할 수 없습니다",
                row_idx, merged.col
            )));
        }

        if count == 0 {
            return Ok(format!(
                "{{\"ok\":true,\"inserted\":0,\"rowCount\":{},\"colCount\":{}}}",
                table.row_count, table.col_count
            ));
        }

        for _ in 0..count {
            table
                .insert_row(row_idx, true)
                .map_err(HwpError::RenderError)?;

            // 갓 생긴 행은 언제나 원본 바로 아래다. 벌마다 같은 자리에 끼우므로
            // 복제본끼리는 순서를 따질 것이 없다 — 모두 같은 내용이다.
            let inserted_row = row_idx + 1;
            for cell in table
                .cells
                .iter_mut()
                .filter(|cell| cell.row == inserted_row)
            {
                let Some(origin) = source.iter().find(|src| src.col == cell.col) else {
                    continue;
                };

                // 자리(행 번호·크기)는 `insert_row` 가 계산한 것을 남기고 내용과
                // 칸 속성만 원본에서 가져온다. 크기까지 덮으면 열 너비를 다시 나눈
                // 결과가 버려진다.
                let mut clone = origin.clone();
                clone.row = cell.row;
                clone.width = cell.width;
                clone.height = cell.height;

                // instanceId 는 문단의 고유 식별자다. `Cell::new_from_template` 도
                // 새 칸의 문단에서 이 값을 지운다 — 복제본이 원본의 신분증까지
                // 들고 다니게 두지 않는다.
                for para in &mut clone.paragraphs {
                    if para.raw_header_extra.len() >= 10 {
                        para.raw_header_extra[6..10].copy_from_slice(&[0, 0, 0, 0]);
                    }
                }

                *cell = clone;
            }
        }

        // `insert_table_row_native` 와 같은 사유: 셀 인덱스 배치가 바뀌었으므로
        // 그 인덱스를 물고 있는 국소 크기 조정 목록은 stale 이다.
        table.dirty = true;
        table.local_resize_cell_widths.clear();
        table.local_resize_cell_heights.clear();
        let row_count = table.row_count;
        let col_count = table.col_count;

        self.document.sections[section_idx].raw_stream = None;
        self.recompose_section(section_idx);
        self.paginate_if_needed();

        self.event_log.push(DocumentEvent::TableRowInserted {
            section: section_idx,
            para: parent_para_idx,
            ctrl: control_idx,
        });

        Ok(format!(
            "{{\"ok\":true,\"inserted\":{},\"rowCount\":{},\"colCount\":{}}}",
            count, row_count, col_count
        ))
    }
}
