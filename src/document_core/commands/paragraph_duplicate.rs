//! 문단 복제 — 서식 문서의 한 수준을 여러 벌로 늘리는 편집 연산.


use crate::document_core::DocumentCore;
use crate::error::HwpError;
use crate::model::event::DocumentEvent;

impl DocumentCore {
    /// 문단을 서식·내용째 복제해 같은 구역의 지정 위치에 넣는다.
    ///
    /// `insert_paragraph_native` 는 문단 모양만 물려받은 **빈** 문단을 만든다. 서식이
    /// `para_shape_id` 에만 있는 문서라면 그것으로 충분하다. 그러나 실제 업무 서식은
    /// 글머리표·번호를 **문단의 첫 글자로** 들고 있는 경우가 많다 — `"  □ "` 처럼.
    /// 게다가 그 앞머리와 본문은 서로 다른 글자 모양(`char_shape_id`)을 쓴다.
    /// 그런 문서에서 한 수준을 여러 벌로 늘리려면 앞머리 글자·글자 모양·그 자리의
    /// 누름틀이 함께 와야 한다. 그래서 문단을 통째로 복제한다.
    ///
    /// 복제본의 누름틀은 원본과 같은 이름을 갖는다. 값을 넣을 때는
    /// [`DocumentCore::set_field_value_by_name_at`] 의 `occurrence` 로 몇 번째인지 고른다.
    ///
    /// # 인자
    ///
    /// * `section_idx` — 구역 인덱스.
    /// * `source_para_idx` — 복제할 원본 문단.
    /// * `dest_para_idx` — 복제본을 넣을 위치. 구역 문단 수와 같으면 맨 끝에 붙는다.
    /// * `count` — 복제 벌 수. 0 이면 아무것도 하지 않는다.
    pub fn duplicate_paragraph_native(
        &mut self,
        section_idx: usize,
        source_para_idx: usize,
        dest_para_idx: usize,
        count: usize,
    ) -> Result<String, HwpError> {
        if section_idx >= self.document.sections.len() {
            return Err(HwpError::RenderError(format!(
                "구역 인덱스 {} 범위 초과 (총 {}개)",
                section_idx,
                self.document.sections.len()
            )));
        }

        let para_count = self.document.sections[section_idx].paragraphs.len();
        if source_para_idx >= para_count {
            return Err(HwpError::RenderError(format!(
                "원본 문단 인덱스 {} 범위 초과 (총 {}개)",
                source_para_idx, para_count
            )));
        }
        if dest_para_idx > para_count {
            return Err(HwpError::RenderError(format!(
                "삽입 위치 {} 범위 초과 (총 {}개, 최대 {})",
                dest_para_idx, para_count, para_count
            )));
        }

        if count == 0 {
            return Ok(format!(
                "{{\"ok\":true,\"inserted\":0,\"paragraphCount\":{}}}",
                para_count
            ));
        }

        self.document.sections[section_idx].raw_stream = None;

        let mut template =
            self.document.sections[section_idx].paragraphs[source_para_idx].clone();

        // 구역/단 나누기 표식은 문단 내용이 아니라 **자리**에 딸린 속성이다. 원본이
        // 구역 첫 문단이면 그 표식까지 복제되어 본문 한가운데서 구역이 새로 시작한다.
        // 복제본은 언제나 본문 문단이므로 표식을 지운다.
        template.column_type = Default::default();
        template.raw_break_type = 0;

        {
            let paragraphs = &mut self.document.sections[section_idx].paragraphs;
            for offset in 0..count {
                paragraphs.insert(dest_para_idx + offset, template.clone());
            }

            // 맨 앞에 끼웠다면 밀려난 문단이 들고 있던 구역 시작 표식을 새 첫 문단으로
            // 옮긴다. 그대로 두면 밀려난 쪽이 계속 구역 시작을 주장해 거기서 쪽이 끊기고,
            // 복제본만 홀로 남은 빈 쪽이 생긴다. (`insert_paragraph_native` 와 같은 처리)
            if dest_para_idx == 0 {
                if let [new_first, .., displaced] = &mut paragraphs[..=count] {
                    new_first.column_type = std::mem::take(&mut displaced.column_type);
                    new_first.raw_break_type = std::mem::take(&mut displaced.raw_break_type);
                }
            }
        }

        let reflow_target = dest_para_idx.saturating_sub(1);
        for offset in 0..count {
            self.reflow_paragraph(section_idx, dest_para_idx + offset);
        }

        let doc_hwp3_layout = self.document.layout_profile().hwp3_layout();
        crate::renderer::composer::recalculate_section_vpos(
            &mut self.document.sections[section_idx].paragraphs,
            reflow_target,
            Some(dest_para_idx..dest_para_idx + count),
            None,
            &self.styles,
            self.dpi,
            doc_hwp3_layout,
        );

        // 오름차순이어야 한다. `insert_composed_paragraph` 는 결국 `Vec::insert(i, _)` 라
        // `i <= len` 이어야 하는데, 내림차순으로 돌면 첫 호출이 가장 큰 인덱스라 범위를
        // 넘어 패닉한다.
        for offset in 0..count {
            self.insert_composed_paragraph(section_idx, dest_para_idx + offset);
        }

        self.paginate_if_needed();
        self.invalidate_page_tree_cache();

        self.event_log.push(DocumentEvent::ParagraphInserted {
            section: section_idx,
            para: dest_para_idx,
        });

        Ok(format!(
            "{{\"ok\":true,\"inserted\":{},\"firstParaIdx\":{},\"paragraphCount\":{}}}",
            count,
            dest_para_idx,
            self.document.sections[section_idx].paragraphs.len()
        ))
    }
}
