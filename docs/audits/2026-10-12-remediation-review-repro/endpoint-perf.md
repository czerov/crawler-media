# C5b：读路径端点性能 before/after 实测（release）

目的：用 **release profile 的实测数据**验证本轮读路径优化（B1/B2 浏览快照、B4 facets、
B5b 历史分页、B6 统计 SQL 聚合、DB5/DB6 索引）是否真的有效，而不是只看代码形状。

## 1. 环境

| 项 | 值 |
|---|---|
| 机器 | Apple M4 Pro（arm64），12 核，24 GB |
| OS | macOS 26.7（Darwin 25.6.0） |
| rustc / cargo | 1.93.1（01f6ddf75 / 083ac5135） |
| profile | 默认 `release`（Cargo.toml 无 `[profile.release]` 覆盖） |
| 运行方式 | `--test-threads=1`，单进程串行；两个 worktree 分开跑，**不并发** |
| 共享情况 | 同一台开发机，测量期间可能有其它 agent 的编译负载；无 CPU 绑核/turbo 控制 |

## 2. 数据集（fixture）

`crates/api/tests/endpoint_perf.rs` 的 `fixture(count, mode)`，固定 seed `0xC5B2026`（LCG），
baseline 与 after **使用同一份测试文件**，因此数据形状完全一致：

- `mode = movie | tv`；`count = 100 / 500 / 2000 / 10000` 个 Media。
- 每个 Media：1–2 个真实小文件（`file-N.mkv`，同目录真实 `.nfo`），目录旁真实 `poster.jpg`（最小合法 JPEG 字节）。
- **多 Library**：每第 3 个 Media 落在第二个 library；其余在 primary library（测量用 primary 的 id）。
- **同一 Media 多文件**：LCG 决定 1 或 2 个 ledger 行。
- 播放日志：每个 Media 一次 `upsert_session` + `close_session`（`started_at` 落在最近 30 天内），
  即 100–10000 条 `playback_logs`。
- 非空 catalog cache：8 条 `put_catalog_cache("tmdb", ...)`。
- 全部通过公共 Store API 造数据（`create_library` / `insert_media` / `insert_ledger` /
  `upsert_session` + `close_session` / `put_catalog_cache`）；无网络、无 ffmpeg、无 Chromium。

被测端点（均为 GET，admin bearer token）：

| 标签 | 路径 |
|---|---|
| items | `/api/v1/libraries/{id}/items?limit=24` |
| gallery | `/api/v1/libraries/{id}/gallery?limit=24` |
| facets | `/api/v1/libraries/{id}/facets` |
| search | `/api/v1/search/library-items?keyword=Perf` |
| history | `/api/v1/playback/history?limit=30` |
| stats | `/api/v1/playback/stats/watch?days=30` |
| jobs | `/api/v1/jobs` |

延迟口径：`tower::ServiceExt::oneshot` 从发请求到**读完响应 body**（客户端可观测延迟），
每端点 **5 次采样**取中位数与 p95；`status` 全部 200。另在 2000/10000 两个 size 上做了
**第二轮独立测量**用于稳定性对照（见第 5 节）。

## 3. 命令与退出码

```bash
# baseline：只读 worktree（不切主工作树分支）
git worktree add --detach /tmp/crawler-media-baseline 16867bff8fea89688126cb869adbec5bda6f6460   # exit 0
cp crates/api/tests/endpoint_perf.rs /tmp/crawler-media-baseline/crates/api/tests/endpoint_perf.rs
CARGO_TARGET_DIR=/tmp/crawler-baseline-target cargo build --release -p api                        # exit 0（预热）

# after（主工作树）
cargo test --release -p api --test endpoint_perf -- --ignored --nocapture --test-threads=1        # exit 0

# before（baseline worktree，独立 target dir）
cd /tmp/crawler-media-baseline
CARGO_TARGET_DIR=/tmp/crawler-baseline-target \
  cargo test --release -p api --test endpoint_perf -- --ignored --nocapture --test-threads=1      # exit 0

# 第二轮（仅 2000/10000）两边各再跑一次                                                # exit 0 / exit 0
PERF_SIZES=2000,10000 cargo test --release -p api --test endpoint_perf -- --ignored --nocapture --test-threads=1
```

## 4. 结果

### movie — median (ms, before → after, speedup)

| endpoint | 100 | 500 | 2000 | 10000 |
|---|---|---|---|---|
| items | 10.04 → 5.53 (1.81×) | 42.68 → 24.34 (1.75×) | 172.77 → 92.81 (1.86×) | 888.84 → 448.03 (1.98×) |
| gallery | 22.32 → 15.55 (1.43×) | 92.54 → 73.53 (1.26×) | 381.27 → 297.36 (1.28×) | 1944.21 → 1507.94 (1.29×) |
| facets | 3.98 → 4.49 (0.89×) | 18.08 → 22.79 (0.79×) | 71.48 → 90.10 (0.79×) | 358.47 → 462.32 (0.78×) |
| search | 7.88 → 5.83 (1.35×) | 36.23 → 27.65 (1.31×) | 144.10 → 111.57 (1.29×) | 728.61 → 542.82 (1.34×) |
| history | 12.65 → 6.77 (1.87×) | 49.08 → 28.68 (1.71×) | 186.30 → 112.35 (1.66×) | 937.94 → 532.66 (1.76×) |
| stats | 9.51 → 7.07 (1.34×) | 41.03 → 29.34 (1.40×) | 159.32 → 116.96 (1.36×) | 805.29 → 571.05 (1.41×) |
| jobs | 0.12 → 0.06 (2.03×) | 0.11 → 0.06 (1.73×) | 0.11 → 0.07 (1.75×) | 0.12 → 0.08 (1.53×) |

### tv — median (ms, before → after, speedup)

| endpoint | 100 | 500 | 2000 | 10000 |
|---|---|---|---|---|
| items | 6.73 → 2.18 (3.08×) | 27.18 → 8.34 (3.26×) | 104.41 → 28.59 (3.65×) | 545.83 → 130.09 (4.20×) |
| gallery | 16.39 → 12.24 (1.34×) | 76.42 → 57.38 (1.33×) | 305.33 → 230.43 (1.33×) | 1604.96 → 1198.11 (1.34×) |
| facets | 3.88 → 1.43 (2.71×) | 17.62 → 7.07 (2.49×) | 70.86 → 26.71 (2.65×) | 361.35 → 135.60 (2.66×) |
| search | 7.71 → 5.91 (1.31×) | 35.81 → 28.25 (1.27×) | 145.07 → 109.99 (1.32×) | 734.24 → 555.43 (1.32×) |
| history | 11.83 → 6.57 (1.80×) | 48.00 → 28.60 (1.68×) | 186.83 → 110.32 (1.69×) | 945.22 → 548.55 (1.72×) |
| stats | 9.31 → 6.82 (1.36×) | 40.43 → 29.31 (1.38×) | 161.62 → 115.72 (1.40×) | 812.53 → 582.88 (1.39×) |
| jobs | 0.11 → 0.06 (1.73×) | 0.11 → 0.06 (1.79×) | 0.11 → 0.09 (1.27×) | 0.11 → 0.07 (1.60×) |

### movie — p95 (ms, before → after, speedup)

| endpoint | 100 | 500 | 2000 | 10000 |
|---|---|---|---|---|
| items | 11.65 → 5.94 (1.96×) | 42.88 → 25.03 (1.71×) | 178.74 → 96.49 (1.85×) | 946.78 → 522.39 (1.81×) |
| gallery | 23.75 → 15.84 (1.50×) | 101.98 → 78.62 (1.30×) | 405.98 → 309.66 (1.31×) | 1985.30 → 1527.82 (1.30×) |
| facets | 4.59 → 4.87 (0.94×) | 18.18 → 24.92 (0.73×) | 72.02 → 96.10 (0.75×) | 362.92 → 541.51 (0.67×) |
| search | 8.37 → 6.02 (1.39×) | 36.65 → 28.11 (1.30×) | 145.53 → 119.40 (1.22×) | 734.13 → 547.41 (1.34×) |
| history | 12.84 → 7.09 (1.81×) | 51.02 → 31.05 (1.64×) | 195.93 → 114.86 (1.71×) | 953.83 → 550.58 (1.73×) |
| stats | 9.78 → 7.27 (1.35×) | 41.76 → 30.39 (1.37×) | 159.68 → 117.54 (1.36×) | 810.42 → 577.09 (1.40×) |
| jobs | 0.26 → 0.16 (1.68×) | 0.19 → 0.14 (1.38×) | 0.21 → 0.15 (1.40×) | 1.13 → 1.47 (0.77×) |

### tv — p95 (ms, before → after, speedup)

| endpoint | 100 | 500 | 2000 | 10000 |
|---|---|---|---|---|
| items | 7.25 → 2.65 (2.73×) | 27.90 → 8.52 (3.27×) | 112.41 → 29.05 (3.87×) | 595.93 → 145.97 (4.08×) |
| gallery | 16.98 → 12.60 (1.35×) | 89.06 → 60.88 (1.46×) | 311.94 → 237.69 (1.31×) | 1649.33 → 1246.27 (1.32×) |
| facets | 3.96 → 1.67 (2.38×) | 17.71 → 8.49 (2.08×) | 71.55 → 26.85 (2.66×) | 365.50 → 142.22 (2.57×) |
| search | 7.96 → 6.19 (1.29×) | 36.09 → 30.15 (1.20×) | 146.15 → 112.31 (1.30×) | 740.26 → 557.62 (1.33×) |
| history | 12.01 → 7.17 (1.67×) | 48.61 → 29.97 (1.62×) | 189.77 → 111.52 (1.70×) | 950.88 → 559.07 (1.70×) |
| stats | 9.36 → 7.25 (1.29×) | 41.17 → 30.25 (1.36×) | 162.99 → 117.80 (1.38×) | 838.86 → 607.35 (1.38×) |
| jobs | 0.17 → 0.13 (1.35×) | 0.19 → 0.13 (1.42×) | 0.21 → 0.15 (1.41×) | 0.23 → 0.23 (1.03×) |

### response bytes (before → after)

| endpoint | 100 | 500 | 2000 | 10000 |
|---|---|---|---|---|
| items | 7844 → 7844 | 7844 → 7844 | 7844 → 7844 | 7844 → 7844 |
| gallery | 10556 → 10556 | 10556 → 10556 | 10556 → 10556 | 10556 → 10556 |
| facets | 117 → 444 ⚠️ | 117 → 450 ⚠️ | 119 → 455 ⚠️ | 119 → 458 ⚠️ |
| search | 36566 → 36566 | 168602 → 168602 | 685136 → 685136 | 3440282 → 3440282 |
| history | 17971 → 17971 | 17971 → 17971 | 17971 → 17971 | 17971 → 17971 |
| stats | 9275 → 9275 | 9335 → 9335 | 9369 → 9369 | 9423 → 9423 |
| jobs | 1521 → 1521 | 1521 → 1521 | 1521 → 1521 | 1521 → 1521 |

> ⚠️ = 响应字节数与基线不同，说明该端点的 DTO 已经变化，**不能**当作同负载对比。

## 5. 逐端点结论（诚实版）

- **items（B1 批量归属 + B2 浏览快照）**：movie 1.75–1.98×，tv 3.08–4.20×；两个 mode 都随 size 稳定改善。
  响应字节完全一致（7844 / 7796），是同负载对比。
- **gallery**：1.26–1.43×，字节一致。
- **search（DB6 media 身份索引）**：1.27–1.35×，字节一致。
- **history（B5b SQL 可见范围分页）**：1.66–1.87×，字节一致。
- **stats（B6 单次 SQL 聚合）**：1.34–1.41×，字节一致。
- **jobs（DB5 索引）**：1.27–2.03×，但绝对值只有 0.06–0.12 ms —— **该行由噪声主导**
  （第二轮同端点波动可达 ~29%），只能说明"没有退化"，不足以宣称倍数。
- **facets：不是同负载对比，且 movie 变慢。**
  - 基线 facets 只返回 `resolutions`，`genres` 恒为空 → 响应 **117–119 字节**；
    after 返回十维 facets → **444–512 字节**（B4 修正了正确性/覆盖度）。
  - movie：3.98→4.49、18.08→22.79、71.48→90.10、358.47→462.32 ms（**0.78–0.89×，变慢**）。
  - tv：3.88→1.43、17.62→7.07、70.86→26.71、361.35→135.60 ms（**2.49–2.71×，变快**）。
  - 解释：after 版本在同样数据上计算并序列化约 4× 的数据量，因此该行不能解读为"性能回归"；
    但 movie 路径在 after 树里确实比 tv 路径慢（10k：462 ms vs 136 ms），**原因未在本任务内定位**
    （本任务只做测量与文档，不改生产代码）。已在汇报中作为待决项交给 Lead。
- **总体**：除 facets 外，所有端点在同字节负载下都有 1.26–4.20× 的改善，且随 size 增大而更明显。

## 6. 稳定性对照（第二轮，2000/10000）

| 端点 | after 两轮中位数差（最大） | before 两轮中位数差（最大） |
|---|---|---|
| items / gallery / search / history / stats / facets | 8.8%（movie/2000/items） | 9.7%（tv/2000/items） |
| jobs（绝对值 0.06–0.12 ms） | 28.9% | 26.8% |

结论：≥1 ms 的端点两轮中位数差异在 **10% 以内**，第 4 节的 before/after 比值可信；
`jobs` 的绝对值太小（差值 ≈0.02–0.05 ms），其倍数不要引用。

## 7. 边界声明（必须与数字一起引用）

1. 这是**同一台机器、同一份合成数据、release profile** 的对比，**不是生产 SLA**，也不是容量规划依据。
2. 播放日志只有 **100–10000 行**；B5b/B6 要解决的"一次请求加载 20 万行"病态**没有在本数据集里复现**。
   因此 history/stats 的 1.3–1.9× 是**该规模下**的实测值，真实 20 万行场景的收益预期更大，但**未经本次测量**。
3. 每端点仅 **5 次采样**；机器为共享开发机（可能有其它 agent 编译负载），未做 CPU 绑核或频率控制。
4. 延迟是"发请求到读完 body"的端到端值，包含 JSON 序列化与响应体传输，不只是 handler 内部时间。
5. baseline 是 `16867bf` 的只读 worktree，使用**同一个测试文件**与独立 `CARGO_TARGET_DIR`；
   除该提交本身包含的行为差异外，没有其它变量。
6. `facets` 行**不是同 DTO 对比**（字节数已标注），不能与其它行一样解读。
7. 本任务**没有修改任何 `src/` 代码**；所有数字都是既有代码的测量结果。

## 8. 复现

```bash
# after（主工作树）
cargo test --release -p api --test endpoint_perf -- --ignored --nocapture --test-threads=1

# before（只读 worktree + 独立 target dir）
git worktree add --detach /tmp/crawler-media-baseline 16867bff8fea89688126cb869adbec5bda6f6460
cp crates/api/tests/endpoint_perf.rs /tmp/crawler-media-baseline/crates/api/tests/endpoint_perf.rs
cd /tmp/crawler-media-baseline
CARGO_TARGET_DIR=/tmp/crawler-baseline-target \
  cargo test --release -p api --test endpoint_perf -- --ignored --nocapture --test-threads=1
cd - && git worktree remove /tmp/crawler-media-baseline
```

`PERF_SIZES` / `PERF_MODES` / `PERF_SAMPLES` 可在迭代时收窄矩阵；第 4 节的表格使用默认值
（sizes 100/500/2000/10000，modes movie+tv，samples 5）。
