# 媒体章节与片头片尾识别技术方案 (STRM 与本地视频通用)

> **历史状态（2026-10-13 整理）：** 初始章节设计历史；其中 STRM 重建后无条件复用媒体信息/标记的承诺已被 [自适应版本化采样设计](<2026-10-08-adaptive-fingerprint-sampling-design.md>) 与 [ADR-0012](<../../adr/0012-library-removal.md>) 的 fresh ledger/fresh probe 政策替代。语义 Media 身份保留不等于旧文件探测/声纹证据永远有效；真实 UI/VidHub 跳过行为尚须实测。
>
> 原设计/ID/测试记录保留；旧执行、清理和提交命令不是当前执行授权，不应直接重跑。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<../plans/2026-10-13-database-api-followup.md>)；未覆盖的历史发现仍须独立对账。

## 1. 背景与目标

本方案旨在解决媒体库中视频文件的**章节提取**与**跳过片头片尾 (Skip Intro/Outro/Recap/Credits)** 能力，重点面向 **Jellyfin 协议客户端（如 Infuse、VidHub、Jellyfin App）** 以及 Web 播放器。

### 核心设计目标：
1. **STRM 与本地文件统一**：底层统一支持 `.strm` 虚拟视频（远程 HTTP/HTTPS URL）与本地磁盘视频（`.mkv`、`.mp4` 等）的探测。
2. **Jellyfin 协议对齐**：在 Jellyfin API 返回标准 `Chapters` 结构与 `MarkerType`（`IntroStart`、`IntroEnd`、`CreditsStart` 等），使 Infuse 等客户端原生弹出「跳过片头」按钮。
3. **三层梯级提取体系 (Cascade Architecture)**：
   - **第一层（首选/云端数据库，零算力）**：对接 [TheIntroDB](https://theintrodb.org/) (v3 API)，根据 TMDB ID + 季集精准拉取社区打点的片头片尾时间戳。
   - **第二层（次选/内嵌元数据，零网络）**：通过 `ffprobe` 提取容器内嵌章节，正则匹配 `Intro` / `OP` / `片头` / `ED` 等。
   - **第三层（终极兜底/本地声纹比对，高开销）**：针对前两层均未命中的无章节剧集，通过 Chromaprint 音频指纹交叉滑动比对提取公共片头。
4. **媒体库级独立控制**：声纹比对作为 CPU/网络密集型功能，提供按媒体库（Library）粒度的独立开关。
5. **抗 STRM 频繁重建机制（原始目标，复用规则已被替代）**：标记的语义键为 `(media_id, season, episode)`，但不能承诺重写/删除重建 `.strm` 后无条件复用媒体信息与采集证据；当前按来源版本校验，新 ledger 安排 fresh probe，手动强制请求重采。见顶部后续设计与 ADR。

---

## 2. 总体架构与梯级提取数据流

```
                           +------------------------------------+
                           |        媒体输入源 (Input)          |
                           |  本地文件 (.mkv) / STRM (URL)      |
                           +-----------------+------------------+
                                             |
                                  解析 TMDB ID + 季集 (SxxExx)
                                             |
                                             v
                           +------------------------------------+
                           |    梯级 1: TheIntroDB 云端数据库   |  <--- 首选 (配置 API Key)
                           |    GET /v3/entries/{tmdb_id}...    |       0 CPU，0 视频下载
                           +-----------------+------------------+
                                             |
                                    [命中]   |   [未收录 / 未配置]
                               +-------------+-------------+
                               |                           |
                               v                           v
                      直接入库 media_markers      +------------------------------------+
                                                  |    梯级 2: 容器内嵌章节提取        |
                                                  |    ffprobe -show_chapters          |
                                                  +-----------------+------------------+
                                                                    |
                                                           [命中]   |   [无章节标记]
                                                      +-------------+-------------+
                                                      |                           |
                                                      v                           v
                                             直接入库 media_markers      +------------------------------------+
                                                                         | 梯级 3: Chromaprint 声纹比对 (兜底)|
                                                                         | (受媒体库开关限制，仅前10分钟音频) |
                                                                         +-----------------+------------------+
                                                                                           |
                                                                                           v
                                                                                  直接入库 media_markers
                                                                                           |
                                             +---------------------------------------------+
                                             |
                                             v
                           +------------------------------------+
                           |   SQLite 语义层持久化缓存          |
                           |   表: media_markers & file_meta    |
                           |   主键: (media_id, season, episode)|
                           +-----------------+------------------+
                                             |
                                             v
                           +------------------------------------+
                           |   Jellyfin 适配层 (jellyfin.rs)     |
                           |   输出标准 Chapters & MarkerType   |
                           +-----------------+------------------+
                                             |
                                +------------+------------+
                                |                         |
                                v                         v
                      Infuse / VidHub 客户端          Web 播放器
                      (弹出「跳过片头」按钮)      (进度条刻度 + 跳过按钮)
```

---

## 3. 详细设计

### 3.1 统一媒体输入源探针 (`library::probe_target`)

目前 `probe_tracks`、`probe_chapters`、`probe_duration`、`extract_frame` 均直接传入本地文件路径。对于 `.strm` 文件，必须先解析出 URL：

```rust
pub enum ProbeTarget {
    Local(PathBuf),
    Remote(String),
}

impl ProbeTarget {
    pub fn from_path(path: &Path) -> Self {
        if path.extension().and_then(|s| s.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("strm")) {
            if let Some(url) = read_first_url(path) {
                return ProbeTarget::Remote(url);
            }
        }
        ProbeTarget::Local(path.to_path_buf())
    }

    /// 应用于 Command 参数：为远程 URL 补充超时和探针限制，避免挂死
    pub fn apply_to_command(&self, cmd: &mut Command) {
        match self {
            ProbeTarget::Local(path) => {
                cmd.arg(path);
            }
            ProbeTarget::Remote(url) => {
                cmd.args([
                    "-timeout", "5000000",       // 5 秒网络连接/读取超时 (微秒)
                    "-analyzeduration", "10000000",
                    "-probesize", "10000000",
                ]).arg(url);
            }
        }
    }
}
```

### 3.2 梯级 1：TheIntroDB (v3 API) 云端数据提取

[TheIntroDB](https://theintrodb.org/) 是开源社区构建的高精度片头片尾时间戳数据库（支持 Intro、Recap、Credits、Preview）。

#### 1. 接口调用规范
- **Base URL**：`https://api.theintrodb.org/v3`
- **鉴权 Header**：`X-API-Key: {theintrodb_api_key}`
- **查询接口**：根据 TMDB TV ID 及季集获取标记：
  ```http
  GET /v3/shows/tmdb/{tmdb_id}/seasons/{season}/episodes/{episode}
  X-API-Key: <YOUR_API_KEY>
  ```
- **典型响应结构**：
  ```json
  {
    "segments": [
      { "type": "recap", "start": 0.0, "end": 45.2 },
      { "type": "intro", "start": 95.5, "end": 185.0 },
      { "type": "credits", "start": 2740.0, "end": 2860.0 }
    ]
  }
  ```
- **映射转换**：
  - `type == "intro"` -> `intro_start_ms = (start * 1000.0) as i64`, `intro_end_ms = (end * 1000.0) as i64`
  - `type == "credits"` -> `outro_start_ms = (start * 1000.0) as i64`
  - `source = "theintrodb"`
- **优势**：
  - 秒级完成，对本地和 STRM 均**完全无需下载任何视频/音频数据**，不占 CPU。

#### 2. 配置存储设计
在刮削配置设置 `metadata.scrape`（`ScrapeConfigSetting`）中增加配置项：
```rust
pub struct ScrapeConfigSetting {
    // 既有字段...
    pub theintrodb_api_key: Option<String>,
    pub theintrodb_enabled: Option<bool>,
}
```
并在系统设置界面中提供 TheIntroDB 开关与 API Key 填空项。

---

### 3.3 梯级 2：容器内嵌章节智能解析 (Chapter Tagging)

当未配置 TheIntroDB、或 TheIntroDB 尚未收录该剧集时，回退到容器内嵌章节提取：

1. **章节提取**：
   通过 `ffprobe -v error -show_chapters -of json` 提取 `start_ms`、`end_ms`、`title`（如果是 `.strm` 则传入远程 URL）。
2. **正则模式识别**：
   - **片头匹配**：
     ```rust
     let intro_re = Regex::new(r"(?i)^(intro|opening|op|theme|prologue|片头|序幕|オープニング|오프닝)").unwrap();
     ```
   - **片尾匹配**：
     ```rust
     let outro_re = Regex::new(r"(?i)^(outro|ending|ed|credits|credit|preview|片尾|演职员|幕后|エンディング|엔딩)").unwrap();
     ```
3. **约束校验**：
   - 片头时长通常在 15s ~ 150s 之间；
   - 片尾章节通常发生在全片时长的后 25% 以后。
4. **耗时**：本地与 STRM 均仅拉取容器头部元数据（几万字节），毫秒级完成。写入时标记 `source = "chapter"`。

---

### 3.4 梯级 3：同季音频指纹交叉比对 (Chromaprint / FPcalc 兜底)

针对前两层均未匹配到的剧集（如 Web-DL、无章节国产剧）：

1. **媒体库级开关控制**：
   - 必须满足对应 `Library` 开启了 `enable_audio_fingerprint: true` 才执行本阶段。
2. **分析时长与参数约束**：
   - **分析时长上限**：仅解出每集**前 10 分钟（600 秒）**的音频流；
   - **采样参数**：单声道（Mono）、16,000Hz 采样率、s16le 格式。
     ```bash
     ffmpeg -v error -timeout 5000000 -ss 0 -t 600 -i <URL/Path> -vn -ac 1 -ar 16000 -f s16le -
     ```
   - **音频拉取体积**：10 分钟 16kHz s16le 音频仅约 19MB，STRM 仅拉取音频流，大幅减少带宽消耗。
3. **指纹生成与比对 (Chromaprint)**：
   - 将 PCM 音频流输入 chromaprint 生成 uint32 指纹数组（每 0.128 秒一个点，10 分钟约 4680 个 uint32 特征值）。
   - 选取同季中至少 2 集（如 E01、E02）的指纹序列；
   - 计算滑动窗口内的汉明距离（Bit Error Rate），容差阈值设定为 12%；
   - 寻找连续匹配时长在 **15s ~ 120s** 之间的最高相似度公共区间；
   - 将匹配到的区间在各集的时间点确定为片头 `[intro_start_ms, intro_end_ms]`，标记 `source = "fingerprint"`。

---

## 4. 数据库设计与缓存隔离（抗 STRM 重建）

为了防止 STRM 重写、删除重建导致重复计算，标记采用**逻辑实体主键**：

### 4.1 数据表定义 (`crates/api/src/store/schema.rs`)

```sql
CREATE TABLE IF NOT EXISTS media_markers (
    media_id TEXT NOT NULL,
    season INTEGER NOT NULL,
    episode INTEGER NOT NULL,

    intro_start_ms INTEGER,
    intro_end_ms INTEGER,
    outro_start_ms INTEGER,
    outro_end_ms INTEGER,

    source TEXT NOT NULL,      -- 'theintrodb' | 'chapter' | 'fingerprint' | 'manual'
    locked INTEGER NOT NULL DEFAULT 0, -- 1: 用户在界面手动微调后锁定，自动任务不可覆盖
    updated_at INTEGER NOT NULL,

    PRIMARY KEY (media_id, season, episode)
);
-- 扩展 file_meta 章节字段，避免重复调用 ffprobe
ALTER TABLE file_meta ADD COLUMN chapters_json TEXT;
```

### 4.2 缓存与读取生命周期：
1. **STRM 文件变动/重建时**：
   - 扫描器扫描到同名 `.strm`，通过正则解析得到 `(media_id, season, episode)`；
   - 查询 `media_markers`：发现已有记录，**直接跳过云端查询、内嵌章节分析与声纹比对**，零网络/CPU 浪费。
2. **强制重新提取时**：
   - 用户在界面触发「强制重新获取元数据与片头」；
   - 清除当前条目的 `file_meta.chapters_json` 及 `media_markers`（或覆盖更新），强制重新执行梯级识别流程。

---

## 5. 配置界面设计与媒体库独立开关

### 5.1 全局云端设置 (`ScrapeConfigSetting`)
在「设置 -> 刮削与整理」中新增【片头片尾云端识别】区块：
- **[x] 启用 TheIntroDB 标记库**
- **TheIntroDB API Key**: `[ tidb_xxxxxxxxxxxxxxxxxxxx ]`（附获取地址 [theintrodb.org](https://theintrodb.org/)）
- 说明：优先通过 TMDB 关联云端片头片尾时间戳，极速精准且不消耗本地算力。

### 5.2 媒体库设置 (`LibrarySettings` / `LibraryFormDialog.tsx`)
在媒体库编辑弹窗中，提供分级开关：
- **[x] 识别内嵌章节与片头片尾**（极速、无性能损耗，STRM/本地均推荐开启）。
- **[ ] 启用音频声纹深度比对 (Chromaprint)**（高级选项）：
  - 说明文案：“仅当 TheIntroDB 与内嵌章节均未命中时生效。将分析同季剧集前 10 分钟音频。STRM 视频会产生额外的音频拉取，建议按需开启。”
  - 并发限制：默认为 1（保护 STRM 网盘挂载源不被限流）。

---

## 6. Jellyfin API 协议适配 (`jellyfin.rs`)

在 `GET /Items` 及 `POST /Items/{id}/PlaybackInfo` 等接口中，输出对齐 Jellyfin 官方规范的 `Chapters` 数组：

```rust
// 组装单个 Item 的 Chapters
let mut chapters = Vec::new();

// 1. 如果有缓存的原生章节
if let Some(list) = cached_chapters {
    for ch in list {
        chapters.push(json!({
            "StartPositionTicks": ch.start_ms * 10_000,
            "Name": ch.title.unwrap_or_else(|| "Chapter".into()),
        }));
    }
}

// 2. 注入片头/片尾 MarkerType
if let Some(marker) = store.get_media_marker(row.media_id, row.season, row.episode)? {
    if let (Some(start), Some(end)) = (marker.intro_start_ms, marker.intro_end_ms) {
        chapters.push(json!({
            "StartPositionTicks": start * 10_000,
            "Name": "片头曲",
            "MarkerType": "IntroStart"
        }));
        chapters.push(json!({
            "StartPositionTicks": end * 10_000,
            "Name": "正片",
            "MarkerType": "IntroEnd"
        }));
    }
    if let Some(outro_start) = marker.outro_start_ms {
        chapters.push(json!({
            "StartPositionTicks": outro_start * 10_000,
            "Name": "片尾",
            "MarkerType": "CreditsStart"
        }));
    }
}

// 保证按 StartPositionTicks 升序排序
chapters.sort_by_key(|c| c["StartPositionTicks"].as_i64().unwrap_or(0));
item_value["Chapters"] = json!(chapters);
```

**客户端交互效果**：
- **Infuse**：解析到 `IntroStart` 并在视频到达该时间戳时，在画面右下角弹出 **「跳过片头 (Skip Intro)」** 按钮；
- **VidHub / Jellyfin 官方客户端**：同步识别到片头区间并支持自动跳过或手动点击跳过。

---

## 7. 实施计划与里程碑

- **Milestone 1: 探针适配、TheIntroDB 与内嵌章节 (P0)**
  - 封装 `ProbeTarget`，让 `probe_chapters`、`probe_tracks`、`probe_duration`、`extract_frame` 兼容 `.strm` 远程 URL。
  - 集成 TheIntroDB v3 客户端（带 API Key 配置与缓存）。
  - 实现内嵌章节正则识别片头片尾（`Chapter Tagging`）。
  - 在 `jellyfin.rs` 响应中补充 `Chapters` 数组及 `MarkerType`，完成 Infuse 客户端端到端联调验证。
- **Milestone 2: 数据库语义存储与抗 STRM 重建 (P0)**
  - 新增 `media_markers` 表与 `file_meta.chapters_json` 字段。
  - 在媒体库扫描与刮削流程中接入持久化查询与缓存复用逻辑。
  - 前端详情页提供「查看片头起止时间」与「强制重新提取」按钮。
- **Milestone 3: 终极兜底 Chromaprint 声纹比对后台任务 (P1)**
  - 集成 `chromaprint` / `fpcalc`，实现基于滑动窗口的同季公共音频提取算法。
  - 在媒体库设置增加「音频声纹深度比对」独立开关及并发限速。
  - 在 Job Center 增加 `library.intro_fingerprint` 异步后台任务。
