mod clipboard;
mod document;
mod footnote_ops;
mod formatting;
mod header_footer_ops;
mod html_import;
mod object_ops;
// 서식 문서의 한 수준을 여러 벌로 늘리기 위한 문단 복제.
mod paragraph_duplicate;
// [#3565] 대형 문서 결함을 이분법으로 좁히기 위한 쪽 범위 추출.
pub mod page_extract;
// 서식 문서의 표를 자료 행 수만큼 늘리기 위한 표 행 복제.
mod table_duplicate;
mod table_ops;
mod text_editing;
