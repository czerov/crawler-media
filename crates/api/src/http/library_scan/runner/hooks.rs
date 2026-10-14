//! 测试专用屏障：用来确定性地复现并发/TOCTOU/故障注入场景。
//!
//! 只在 `cfg(test)` 下编译；生产路径不引用这里。

/// 测试专用屏障：在「读取 Library 配置」与「捕获 scope」之间按 Library 执行一次。
///
/// 用来确定性地复现 P2-4 的 TOCTOU（准入后 PATCH roots）与 P3-1 的轮内并入。
pub(crate) mod test_scope_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    type Hook = Box<dyn FnOnce() + Send + 'static>;
    static HOOKS: Mutex<Option<HashMap<String, Hook>>> = Mutex::new(None);

    pub(crate) fn set(library_id: &str, hook: Hook) {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(library_id.to_string(), hook);
    }

    pub(crate) fn run(library_id: &str) {
        let hook = HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .remove(library_id);
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// 测试专用屏障：在每个 chunk 的文件版本观察之前执行一次。
///
/// 用来证明慢文件观察（模拟慢 NAS stat）期间不持有 Store 锁（P2-NEW-2）。
pub(crate) mod test_observe_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    type Hook = Box<dyn FnOnce() + Send + 'static>;
    static HOOKS: Mutex<Option<HashMap<String, Hook>>> = Mutex::new(None);

    pub(crate) fn set(library_id: &str, hook: Hook) {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(library_id.to_string(), hook);
    }

    pub(crate) fn run(library_id: &str) {
        let hook = HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .remove(library_id);
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// 测试专用屏障：在 chunk 台账已提交、元数据解析之前执行一次，可注入一个失败响应。
///
/// 用来复现「ledger 已提交但后续阶段失败」的故障，验证失败报告仍带上该 chunk 的
/// 已提交前缀（P3-3 fault injection）。
pub(crate) mod test_metadata_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::response::Response;

    type Hook = Box<dyn FnOnce() -> Option<Response> + Send + 'static>;
    static HOOKS: Mutex<Option<HashMap<String, Hook>>> = Mutex::new(None);

    pub(crate) fn set(op_id: &str, hook: Hook) {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(op_id.to_string(), hook);
    }

    pub(crate) fn run(op_id: &str) -> Option<Response> {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .remove(op_id)
            .and_then(|hook| hook())
    }
}
