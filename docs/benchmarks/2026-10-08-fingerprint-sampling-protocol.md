# 2026-10-08 Fingerprint Sampling Protocol and Benchmark

> **历史状态（2026-10-13 整理）：** benchmark 协议/报告支持已随 b7730be 实现；本文是度量约束，不是整季优化已达标的结果报告。fixture、报告测试和缓存再分析不能替代真实 STRM 冷采三组配对、人工真值和准确性/性能门槛。
>
> 原设计/ID/测试记录保留；旧执行、清理和提交命令不是当前执行授权，不应直接重跑。当前依据：[数据库/API review](<../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<../superpowers/plans/2026-10-13-database-api-followup.md>)；未覆盖的历史发现仍须独立对账。

## Protocol Overview
The adaptive fingerprint sampling protocol evaluates the performance, accuracy, and network/disk byte reduction achieved by adaptive window probing vs. full-window extraction across entire seasons.

### Invariants & Metrics
1. **Byte Measurement**:
   - `input_bytes` is populated only when exact origin/AVIO body bytes are reliably measured.
   - If bytes cannot be accurately verified (e.g. unknown network pipe), `input_bytes` is `None` / `null` and `measurement_complete = false`.
2. **Accuracy Gate**:
   - Boundary deviation from ground truth must be $\le 2000$ ms ($2$ seconds).
   - False positive rate must be $0$.
3. **Execution Commands**:
   - Python report evaluation:
     ```bash
     python3 scripts/fingerprint-bench/report_cli.py --results <results_dir> --output <report.md>
     ```
   - Rust unit tests:
     ```bash
     cargo test -p api --test adaptive_benchmark_report
     python3 -m unittest discover -s scripts/fingerprint-bench -p 'test_*.py'
     ```
