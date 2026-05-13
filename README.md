# check-paper

`check-paper` 用来分析 `paper/{作者}/{论文目录}/article.md` 中的本地论文库，并通过 LLM 和 Telegram bot 做可追溯问答。

当前采用两层结构：

1. 离线理解层：扫描新增或变更论文，清洗正文，分块入库，并调用 LLM 生成每篇论文的结构化理解。
2. 问答层：优先使用论文理解回答；当问题需要具体证据、数值或理解层不足时，回到原文 chunk 检索，并可结合 FTS、事实、画像和向量路由做混合检索。

## 配置

本地开发先构建命令：

```bash
cargo build
```

构建后会有三个命令入口：短命令 `ppc`，完整命令 `paper-check`，兼容命令 `check-paper`。

开发时可以用：

```bash
cargo run --bin ppc -- config --show
```

安装到本机：

```bash
cargo install --path .
```

推荐用命令写入本地配置文件 `.paper-check.json`：

```bash
ppc config
ppc llm config
ppc tg config
```

这些配置命令会逐项提示输入，例如 `ppc config` 会依次询问 `db-path`、`default-author`、`proxy`。`default-author` 可留空，之后在命令里用 `--author` 指定作者。`proxy` 可选，格式例如 `http://127.0.0.1:7890` 或 `socks5://127.0.0.1:7890`，会用于 LLM、Embedding 和 Telegram 请求。

`ppc llm config` 会依次询问 `base-url`、`api-key`、`model`、`timeout-secs`、`tls-backend` 和可选的 token 成本参数。默认 LLM 请求超时为 180 秒，`tls-backend` 可选 `rustls` 或 `native`。配置后可以用小请求检查连通性：

```bash
ppc llm check
```

`ppc tg config` 会询问 `bot-token` 和可选的 `chat-ids`；多个 chat id 用英文逗号分隔，不填则不限制聊天。

查看配置：

```bash
ppc config --show
ppc llm config --show
ppc tg config --show
ppc tg status
```

`ppc tg status` 会检查 Telegram bot token、允许的 chat id、代理配置，以及 Telegram `getMe` API 连通性。它用于确认 bot 配置和 Telegram API 是否可达；实际轮询服务仍由 `ppc serve-telegram` 启动。

`.paper-check.json` 已加入 `.gitignore`。环境变量仍然可用，并且优先级高于本地配置文件。`CHECK_PAPER_PAPER_ROOT` 不需要配置，默认读取当前目录下的 `paper/`。

`CHECK_PAPER_LLM_BASE_URL` 支持 OpenAI-compatible `/chat/completions` 接口。

向量检索默认关闭。需要启用远端 OpenAI-compatible embeddings 时，设置：

```bash
CHECK_PAPER_EMBEDDING_PROVIDER=openai-compatible
CHECK_PAPER_EMBEDDING_BASE_URL=https://api.openai.com/v1
CHECK_PAPER_EMBEDDING_API_KEY=...
CHECK_PAPER_EMBEDDING_MODEL=...
```

可选项包括 `CHECK_PAPER_EMBEDDING_MODEL_VERSION`、`CHECK_PAPER_EMBEDDING_TIMEOUT_SECS`、`CHECK_PAPER_EMBEDDING_TLS_BACKEND`、`CHECK_PAPER_EMBEDDING_BATCH_SIZE`。

## 常用命令

```bash
ppc authors
ppc scan --author "Ruqiang ZOU"
ppc ingest --author "Ruqiang ZOU"
ppc analyze --author "Ruqiang ZOU" --limit 5
ppc embed --author "Ruqiang ZOU"
ppc ask --author "Ruqiang ZOU" "这个人的主要研究贡献是什么？"
ppc serve-telegram
```

`ppc authors` 会列出当前数据库中已经入库的作者和论文数。忘记作者名时先运行它，再把列表中的名字传给 `--author`；也可以用 `ppc config` 设置默认作者。
如果运行 `ppc status`、`ppc ask`、`ppc analyze` 等命令时没有传 `--author` 且没有默认作者，CLI 会直接在错误信息里带上可用作者列表和下一步命令。

一键同步新增论文并分析：

```bash
ppc sync --author "Ruqiang ZOU"
```

`ppc sync` 会显示入库和分析进度。分析阶段每篇论文都会显示独立的处理进度，当前论文完成后再进入下一篇；每篇论文会自动重试，单篇失败会记录后继续处理后续论文，最后汇总失败列表。之后重新运行 `ppc sync` 会继续重试未成功分析的论文。

分析和维护常用参数：

```bash
ppc analyze --author "Ruqiang ZOU" --failed-only
ppc analyze --author "Ruqiang ZOU" --stale-only
ppc analyze --author "Ruqiang ZOU" --force --skip-author-profile
ppc profile --author "Ruqiang ZOU"
ppc profile --author "Ruqiang ZOU" --rebuild
```

任务、状态和日志：

```bash
ppc status --author "Ruqiang ZOU"
ppc jobs --author "Ruqiang ZOU" --status failed
ppc jobs --author "Ruqiang ZOU" --retry-failed
ppc jobs --cancel 123
ppc logs qa --author "Ruqiang ZOU" --last 20
ppc logs qa --errors
ppc logs jobs --failed
ppc logs jobs --errors
```

评测：

```bash
ppc eval --fixture tests/fixtures/golden_questions.json --top-k 8
ppc eval --fixture tests/fixtures/golden_questions.json --trace
```

## Telegram 用法

```text
/start
/help
/authors
/use_author
/use_author Ruqiang ZOU
/current_author
/profile
/profile Ruqiang ZOU
/status
/status detail
/jobs
/jobs failed
/sources
/sources full
/cancel job_id
/ask 这个人的 MOF 相关成果有哪些？
/ask Ruqiang ZOU | 固态电池方向有什么代表性论文？
```

`/authors` 和不带参数的 `/use_author` 会列出当前数据库中已经入库的作者，并提示回复序号选择作者，例如回复 `1`。群聊中选择序号也需要艾特 bot，例如 `@你的Bot用户名 1`。如果直接提问但当前 chat 没有默认作者，bot 也会先列出作者；选中后会继续回答刚才的问题。

如果没有在命令中指定作者，bot 会优先使用当前 chat 通过 `/use_author` 或 `/authors` 设置的作者；未设置时使用 `CHECK_PAPER_DEFAULT_AUTHOR`。私聊中设置默认作者后，可以直接发送问题。

群聊中需要艾特 bot 才会响应，例如：

```text
@你的Bot用户名 这篇论文讲什么？
/ask@你的Bot用户名 这个人的 MOF 相关成果有哪些？
```

如果配置了 `TELEGRAM_CHAT_IDS`，bot 只会响应这些私聊或群聊；群聊 chat id 通常是负数。

## 数据位置

默认数据库：

```text
data/check_paper.sqlite
```

数据库里保存：

- 论文元数据和 source hash
- 清洗后的正文 chunk
- FTS 检索索引
- chunk embedding 和 embedding 版本信息
- 每篇论文的 LLM 理解 JSON
- 作者级聚合画像 JSON
- 分析任务队列、任务状态历史和失败原因
- QA 日志、引用快照、token 用量和估算成本
