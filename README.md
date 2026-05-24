# check-paper

`check-paper` 是本地论文库 Evidence QA 服务。它读取：

```text
paper/<AUTHOR>/<PAPER_ID>/article.md
```

然后完成论文入库、清洗、分块、LLM 结构化理解、作者画像聚合，并通过 CLI 或 Telegram bot 做带来源记录的问答。

## 快速开始

```bash
cargo build
target/debug/ppc config
target/debug/ppc llm config
target/debug/ppc tg config
```

常用检查：

```bash
target/debug/ppc config --show
target/debug/ppc llm check
target/debug/ppc tg status
target/debug/ppc authors
```

`.paper-check.json` 已加入 `.gitignore`。环境变量优先级高于配置文件。常用配置项：

- `CHECK_PAPER_DB_PATH`：SQLite 路径，默认 `data/check_paper.sqlite`
- `CHECK_PAPER_DEFAULT_AUTHOR`：默认作者
- `CHECK_PAPER_QA_PROFILE_VERSION`：`v1`、`v2` 或 `auto`
- `CHECK_PAPER_PROXY`：LLM、Embedding、Telegram 共用代理
- `CHECK_PAPER_LLM_BASE_URL` / `CHECK_PAPER_LLM_API_KEY` / `CHECK_PAPER_LLM_MODEL`
- `TELEGRAM_BOT_TOKEN` / `TELEGRAM_CHAT_IDS` / `TELEGRAM_ADMIN_USER_IDS`

## 论文数据

每篇论文一个目录：

```text
paper/Ruqiang ZOU/2021-10-1016-j-apcatb-2020-119591/
  article.md
  fetch-result.json
  source.pdf
```

`article.md` 是必须文件；`fetch-result.json` 和 `source.pdf` 是推荐保留的来源证据。PDF 导入流程见 [skills/pdf-paper-source/SKILL.md](skills/pdf-paper-source/SKILL.md)。

## CLI 用法

```bash
target/debug/ppc authors
target/debug/ppc scan --author "Ruqiang ZOU"
target/debug/ppc ingest --author "Ruqiang ZOU"
target/debug/ppc sync --author "Ruqiang ZOU"
target/debug/ppc ask --author "Ruqiang ZOU" "这个人的主要研究贡献是什么？"
```

分析、V2 profile 和向量检索：

```bash
target/debug/ppc analyze --author "Ruqiang ZOU" --limit 5
target/debug/ppc classify --author "Ruqiang ZOU"
target/debug/ppc extract --author "Ruqiang ZOU" --v2
target/debug/ppc comprehend --author "Ruqiang ZOU" --v2
target/debug/ppc comprehend --author "Ruqiang ZOU" --v2 --author-profile
target/debug/ppc embed --author "Ruqiang ZOU"
target/debug/ppc profile --author "Ruqiang ZOU" --v2
```

日志和维护：

```bash
target/debug/ppc status --author "Ruqiang ZOU"
target/debug/ppc jobs --author "Ruqiang ZOU" --status failed
target/debug/ppc logs qa --last 20
target/debug/ppc logs telegram --summary
target/debug/ppc backup
```

更完整的使用流程见 [skills/use-check-paper/SKILL.md](skills/use-check-paper/SKILL.md)。

## Telegram

启动本地 polling 服务：

```bash
mkdir -p data
nohup target/debug/ppc serve-telegram > data/ppc-telegram.log 2>&1 &
```

检查：

```bash
target/debug/ppc tg status
target/debug/ppc tg health
tail -n 80 data/ppc-telegram.log
```

群聊中需要显式 @ bot：

```text
@你的Bot用户名 /status
@你的Bot用户名 /authors
@你的Bot用户名 这个人的主要研究贡献是什么？
@你的Bot用户名 /sources
```

`/sync`、`/analyze`、`/embed`、`/comprehend` 等管理命令只允许 `TELEGRAM_ADMIN_USER_IDS` 中的用户在群聊执行。

## 回归和上线证据

```bash
scripts/regression-check.sh
scripts/eval-v2-gate.sh
scripts/v2-default-readiness.sh "Ruqiang ZOU"
scripts/v2-default-switch-plan.sh "Ruqiang ZOU"
scripts/telegram-deploy-evidence.sh
scripts/production-readiness-evidence.sh "Ruqiang ZOU"
scripts/evidence-ledger.sh
```

`.github/workflows/regression.yml` 会在 push、pull request、每周定时和手动触发时运行同一条 regression gate，并上传 evidence artifact。

## 本仓库 Skills

- [skills/use-check-paper/SKILL.md](skills/use-check-paper/SKILL.md)：指导其他 agent 使用本服务，包括配置、启动、CLI、Telegram、日志和回归。
- [skills/pdf-paper-source/SKILL.md](skills/pdf-paper-source/SKILL.md)：把用户选择的 PDF 文献整理成 `paper/<AUTHOR>/<PAPER_ID>/article.md` 数据来源。

全局可发现版本也可安装到 `/Users/hanlife02/.codex/skills/`；仓库内版本用于随项目一起维护和复制。
