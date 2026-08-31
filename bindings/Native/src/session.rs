//! Document session handles for the C ABI.
//!
//! 기존 진입점(`rhwp_export_text` 등)은 호출마다 파일을 다시 파싱한다. 한 문서를
//! 여러 번 만지는 편집 작업에서는 그 비용이 그대로 반복되고, 무엇보다 **편집 결과가
//! 호출 사이에 남지 않는다.** 그래서 문서를 세션으로 들고 있는 표면을 따로 둔다.
//!
//! 핸들은 포인터가 아니라 **정수 인덱스**다. 호출 측의 잘못된 값이 곧바로 정의되지
//! 않은 동작이 되지 않게 하기 위함이다. 유효하지 않은 핸들은 오류로 응답한다.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use rhwp_core::wasm_api::HwpDocument;

/// 열려 있는 문서들. 키는 핸들 값이다.
fn registry() -> &'static Mutex<HashMap<u64, HwpDocument>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, HwpDocument>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 다음에 나눠 줄 핸들. 0 은 "유효하지 않음"으로 예약한다.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// 문서를 등록하고 핸들을 돌려준다.
pub(crate) fn insert(document: HwpDocument) -> Result<u64, String> {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    lock()?.insert(handle, document);
    Ok(handle)
}

/// 핸들을 해제한다. 없는 핸들은 조용히 무시한다(이중 해제 허용).
pub(crate) fn remove(handle: u64) {
    if let Ok(mut documents) = lock() {
        documents.remove(&handle);
    }
}

/// 핸들이 가리키는 문서로 작업한다.
///
/// 잠금을 쥔 채로 클로저를 부른다. 같은 문서에 대한 동시 편집을 직렬화하기 위해서다.
/// `DocumentCore` 는 내부 가변 상태(페이지네이션 캐시 등)를 들고 있어 동시 접근이
/// 안전하지 않다.
pub(crate) fn with<T>(
    handle: u64,
    f: impl FnOnce(&mut HwpDocument) -> Result<T, String>,
) -> Result<T, String> {
    let mut documents = lock()?;
    match documents.get_mut(&handle) {
        Some(document) => f(document),
        None => Err(format!(
            "유효하지 않은 문서 핸들입니다: {handle}. 이미 닫혔거나 열린 적이 없습니다."
        )),
    }
}

/// 현재 열려 있는 문서 수. 누수 점검용이다.
pub(crate) fn count() -> usize {
    lock().map(|documents| documents.len()).unwrap_or(0)
}

/// 레지스트리 잠금.
///
/// 다른 호출이 패닉한 채 잠금을 놓고 갔더라도 이 레지스트리는 `HashMap` 하나뿐이라
/// 불변식이 깨지지 않는다. 그래서 poison 을 오류로 올리지 않고 그대로 이어 쓴다 —
/// 여기서 막으면 그 뒤의 모든 문서 작업이 프로세스가 죽을 때까지 실패한다.
fn lock() -> Result<std::sync::MutexGuard<'static, HashMap<u64, HwpDocument>>, String> {
    Ok(registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
}
