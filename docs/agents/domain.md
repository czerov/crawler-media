# 领域文档

工程技能在探索代码库时应如何消费本仓库的领域文档。

## 探索之前先读这些

- 仓库根的 [CONTEXT.md](<../../CONTEXT.md>) —— 领域词汇；当前需求以用户请求为准，实现以当前代码和证据为准
- [ADR](<../adr>) —— 仅阅读与你将工作的区域相关的决策
- [roadmap](<roadmap.md>) —— 阶段历史归档，不是当前状态或 API 规范；仅当用户请求引用时读取
- [AGENTS.md](<../../AGENTS.md>) —— 工作范围、测试 seam、Rust 体积限制
- 当前数据库/API 状态与已知剩余工作：[review](<../audits/2026-10-13-database-api-review.md>)、[follow-up](<../superpowers/plans/2026-10-13-database-api-followup.md>)。未覆盖的历史审计发现仍需逐 ID 对账，不因归档而视为关闭。

若这些文件中有任何不存在，**静默继续**。不要标记其缺失；不要主动建议创建它们。`/domain-modeling` skill（经 `/grill-with-docs` 与 `/improve-codebase-architecture` 触达）在术语或决策实际被解析时才惰性创建它们。

## 文件结构

单上下文仓库：

```
/
├── CONTEXT.md
├── docs/adr/
├── docs/agents/roadmap.md
└── crates/
```

## 使用 glossary 的词汇

当你的输出命名领域概念（issue 标题、重构提案、测试名）时，使用 `CONTEXT.md` 中定义的术语。不要漂移到 glossary 明确回避的同义词。

如果 glossary 中还没有你需要的概念，那是一个信号——要么你在发明项目不用的语言（重新考虑），要么存在真实缺口（为 `/domain-modeling` 记一笔）。

## 标记 ADR 冲突

若你的输出与既有 ADR 冲突，明确标出，而不是静默覆盖。
