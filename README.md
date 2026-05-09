# check-paper

`check-paper` 用来分析 `paper/{作者}/{论文目录}/article.md` 中的本地论文库，并通过 LLM 和 Telegram bot 做可追溯问答。

第一版采用两层结构：

1. 离线理解层：扫描新增或变更论文，清洗正文，分块入库，并调用 LLM 生成每篇论文的结构化理解。
2. 问答层：优先使用论文理解回答；当问题需要具体证据、数值或理解层不足时，回到原文 chunk 检索。

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

这些配置命令会逐项提示输入，例如 `ppc config` 会依次询问 `db-path`、`default-author`、`proxy`。`proxy` 可选，格式例如 `http://127.0.0.1:7890` 或 `socks5://127.0.0.1:7890`，会用于 LLM 和 Telegram 请求。`ppc llm config` 会依次询问 `base-url`、`api-key`、`model`。`ppc tg config` 会询问 `bot-token` 和可选的 `chat-ids`；多个 chat id 用英文逗号分隔，不填则不限制聊天。

查看配置：

```bash
ppc config --show
ppc llm config --show
ppc tg config --show
```

`.paper-check.json` 已加入 `.gitignore`。环境变量仍然可用，并且优先级高于本地配置文件。`CHECK_PAPER_PAPER_ROOT` 不需要配置，默认读取当前目录下的 `paper/`。

`CHECK_PAPER_LLM_BASE_URL` 支持 OpenAI-compatible `/chat/completions` 接口。

## 常用命令

```bash
ppc scan --author "Ruqiang ZOU"
ppc ingest --author "Ruqiang ZOU"
ppc analyze --author "Ruqiang ZOU" --limit 5
ppc ask --author "Ruqiang ZOU" "这个人的主要研究贡献是什么？"
ppc serve-telegram
```

一键同步新增论文并分析：

```bash
ppc sync --author "Ruqiang ZOU"
```

## Telegram 用法

```text
/start
/profile
/profile Ruqiang ZOU
/ask 这个人的 MOF 相关成果有哪些？
/ask Ruqiang ZOU | 固态电池方向有什么代表性论文？
```

如果没有在命令中指定作者，bot 会使用 `CHECK_PAPER_DEFAULT_AUTHOR`。

私聊中可以直接发送命令或问题。群聊中需要艾特 bot 才会响应，例如：

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
- 每篇论文的 LLM 理解 JSON
- 作者级聚合画像 JSON
