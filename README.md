# check-paper

`check-paper` 用来分析 `paper/{作者}/{论文目录}/article.md` 中的本地论文库，并通过 LLM 和 Telegram bot 做可追溯问答。

当前采用两层结构：

1. 离线理解层：扫描新增或变更论文，清洗正文，分块入库，并调用 LLM 生成每篇论文的结构化理解。
2. 问答层：优先使用论文理解回答；当问题需要具体证据、数值或理解层不足时，回到原文 chunk 检索，并可结合 FTS、事实、画像和向量路由做混合检索。

默认问答是对话式输出：宏观问题优先使用已经分析好的 paper profiles，并把当前相关论文的 profile-grounding chunks 一起提供给 LLM；当问题要求“依据、原文、实验条件、数值、图表、方法细节”等具体信息时，系统会回到 article.md 切出的 source chunks 检索。回答里的 evidence 会写入 QA 日志；用户明确要求依据，或在 Telegram 使用 `/sources`，才会展开完整来源列表。

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

这些配置命令会逐项提示输入，例如 `ppc config` 会依次询问 `db-path`、`default-author`、`qa-profile-version`、`proxy`。`default-author` 可留空，之后在命令里用 `--author` 指定作者。`qa-profile-version` 可选 `v1`、`v2` 或 `auto`：`auto` 会在目标作者已有 V2 paper profiles 时使用 V2，否则回退到 V1。`proxy` 可选，格式例如 `http://127.0.0.1:7890` 或 `socks5://127.0.0.1:7890`，会用于 LLM、Embedding 和 Telegram 请求。

`ppc llm config` 会依次询问 `base-url`、`api-key`、`model`、`timeout-secs`、`tls-backend` 和可选的 token 成本参数。默认 LLM 请求超时为 180 秒，`tls-backend` 可选 `rustls` 或 `native`。配置后可以用小请求检查连通性：

```bash
ppc llm check
```

`ppc tg config` 会询问 `bot-token`、可选的 `chat-ids` 和可选的 `admin-user-ids`；多个 id 用英文逗号分隔，不填 `chat-ids` 则不限制聊天。

查看配置：

```bash
ppc config --show
ppc llm config --show
ppc tg config --show
ppc tg status
ppc tg health
ppc tg health --strict
ppc tg health --strict --notify
ppc tg health --strict --notify --notify-chat-id -1001234567890
ppc tg service-template --kind launchd
ppc tg service-template --kind launchd-health
ppc tg service-template --kind systemd
ppc tg service-template --kind logrotate
ppc tg service-install --kind launchd --dry-run
ppc tg service-install --kind launchd-health --dry-run
ppc tg service-install --kind launchd --force
ppc tg service-check --kind launchd
ppc tg service-check --kind launchd-health
ppc tg service-check --kind logrotate
scripts/telegram-health-schedule-template.sh launchd
scripts/telegram-health-schedule-template.sh systemd
scripts/telegram-health-schedule-template.sh cron
scripts/telegram-logrotate-schedule-template.sh launchd
scripts/telegram-logrotate-schedule-template.sh systemd
scripts/telegram-logrotate-schedule-template.sh cron
scripts/telegram-deploy-evidence.sh
scripts/production-bootstrap-plan.sh launchd "Ruqiang ZOU"
scripts/production-readiness-evidence.sh "Ruqiang ZOU"
```

`ppc tg status` 会检查 Telegram bot token、允许的 chat id、管理员 user id、代理配置，以及 Telegram `getMe` API 连通性。`ppc tg health` 会读取本地 SQLite 中的 Telegram polling heartbeat，判断 `ppc serve-telegram` 最近是否仍在写入心跳。`status` 用于确认 Telegram API 是否可达，`health` 用于确认本机 polling 进程是否新鲜。`ppc tg health --strict` 适合外部监控或定时任务：heartbeat 缺失或 stale 时会返回非零退出码。加 `--notify` 后，失败检查会用已配置的 Telegram bot 向 `TELEGRAM_CHAT_IDS` 发送告警；也可以重复传 `--notify-chat-id` 指定告警 chat。`ppc tg service-template` 只打印 macOS `launchd`、Linux `systemd` 或 `logrotate` 模板；`launchd-health` 会生成每 300 秒执行 `ppc tg health --strict --notify` 的 macOS 用户级定时任务。`ppc tg service-install` 会把模板写到用户级默认路径，例如 macOS `~/Library/LaunchAgents/com.check-paper.telegram.plist`、`~/Library/LaunchAgents/com.check-paper.telegram-health.plist` 或 Linux `~/.config/systemd/user/check-paper-telegram.service`。安装命令默认不覆盖已有文件，需加 `--force`；它也不会自动启动系统服务，只会打印后续 `launchctl`、`systemctl --user` 或 `logrotate` 命令。`ppc tg service-check` 是只读检查，会确认默认或指定 `--output` 路径是否存在、是否与当前 `--bin` / `--workdir` / `--log` 生成的模板一致，并打印下一步 bootstrap、status 或 logrotate 验证命令。三者都可用 `--bin`、`--workdir`、`--log` 覆盖默认路径。`scripts/telegram-health-schedule-template.sh` 可打印 macOS `launchd`、Linux `systemd timer` 或 `cron` 模板，用于定时执行 `ppc tg health --strict --notify`；默认每 300 秒或 cron 每 5 分钟执行一次，可用 `CHECK_PAPER_PPC_BIN`、`CHECK_PAPER_WORKDIR`、`CHECK_PAPER_TG_HEALTH_LOG`、`CHECK_PAPER_TG_HEALTH_INTERVAL_SECONDS` 和 `CHECK_PAPER_TG_HEALTH_CRON_SCHEDULE` 调整。`scripts/telegram-logrotate-schedule-template.sh` 可打印 macOS `launchd`、Linux `systemd timer` 或 `cron` 模板，用于定时执行已安装的 Telegram logrotate 配置；默认读取 `~/.config/logrotate.d/check-paper-telegram`，可用 `CHECK_PAPER_TG_LOGROTATE_CONFIG`、`CHECK_PAPER_TG_LOGROTATE_STATUS`、`CHECK_PAPER_TG_LOGROTATE_LOG`、`CHECK_PAPER_TG_LOGROTATE_HOUR` 和 `CHECK_PAPER_TG_LOGROTATE_MINUTE` 调整。`scripts/telegram-deploy-evidence.sh` 会生成只读 Markdown 证据，汇总 `tg status`、service-check、`tg health --strict`、Telegram summary/trend、logrotate dry-run，以及可用时的 `launchctl print`、`systemctl --user status` 或 cron health/logrotate schedule signals；默认写到 `/private/tmp/check-paper-telegram-deploy`，可用 `CHECK_PAPER_TG_DEPLOY_REPORT_DIR`、`CHECK_PAPER_TG_SERVICE_KINDS`、`CHECK_PAPER_TG_DEPLOY_TREND_DAYS` 和 `CHECK_PAPER_TG_DEPLOY_FAIL_ON_HOLD=1` 调整。`scripts/production-bootstrap-plan.sh launchd|systemd|cron` 会生成只读目标机 bootstrap plan 和模板包，汇总 Telegram service、Telegram health schedule、Telegram logrotate、regression scheduler、Telegram logrotate schedule、apply checklist 和验证命令；默认写到 `/private/tmp/check-paper-production-bootstrap`，可用 `CHECK_PAPER_PRODUCTION_BOOTSTRAP_REPORT_DIR` 调整，它不会安装服务、改 crontab、bootstrap launchd、enable systemd 或改变默认 profile。`scripts/production-readiness-evidence.sh` 会只读运行 V2 default readiness、regression deploy evidence 和 Telegram deploy evidence，并汇总成目标机总验收报告；默认写到 `/private/tmp/check-paper-production-readiness`，可用 `CHECK_PAPER_PRODUCTION_READINESS_REPORT_DIR` 调整，只有三份子报告都为 `ready` 时总报告才是 `ready`，如需 `hold` 时非零退出可设置 `CHECK_PAPER_PRODUCTION_READINESS_FAIL_ON_HOLD=1`。

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
运行 `ppc embed --author "Ruqiang ZOU" --limit 20 --max-attempts 3` 会按 batch size 分批写入 chunk embedding；每个 batch 失败会自动重试，最终失败会记录到 `embedding_jobs`，之后可重新运行 `ppc embed` 继续处理未成功的 chunk。

## 常用命令

```bash
ppc authors
ppc scan --author "Ruqiang ZOU"
ppc ingest --author "Ruqiang ZOU"
ppc analyze --author "Ruqiang ZOU" --limit 5
ppc classify --author "Ruqiang ZOU"
ppc extract --author "Ruqiang ZOU" --v2
ppc comprehend --author "Ruqiang ZOU" --v2 --dry-run
ppc comprehend --author "Ruqiang ZOU" --v2
ppc comprehend --author "Ruqiang ZOU" --v2 --author-profile --dry-run
ppc comprehend --author "Ruqiang ZOU" --v2 --author-profile
ppc embed --author "Ruqiang ZOU"
ppc ask --author "Ruqiang ZOU" "这个人的主要研究贡献是什么？"
ppc backup
ppc preflight --author "Ruqiang ZOU" --limit 5
ppc serve-telegram
```

`ppc authors` 会列出当前数据库中已经入库的作者和论文数，也会显示 `paper/` 下已发现但尚未入库的作者。忘记作者名时先运行它，再把列表中的名字传给 `--author`；也可以用 `ppc config` 设置默认作者。
如果运行 `ppc status`、`ppc ask`、`ppc analyze` 等命令时没有传 `--author` 且没有默认作者，CLI 会直接在错误信息里带上可用作者列表和下一步命令。

一键同步新增论文并分析：

```bash
ppc sync --author "Ruqiang ZOU"
```

`ppc sync` 会显示入库和分析进度。分析阶段每篇论文都会显示独立的处理进度，当前论文完成后再进入下一篇；每篇论文会自动重试，单篇失败会记录后继续处理后续论文，最后汇总失败列表。之后重新运行 `ppc sync` 会继续重试未成功分析的论文。

V2 理解层的第一步是 chunk 分类，不会改变当前问答默认行为：

```bash
ppc classify --author "Ruqiang ZOU"
ppc classify --author "Ruqiang ZOU" --dry-run
ppc classify --author "Ruqiang ZOU" --force
ppc extract --author "Ruqiang ZOU" --v2
ppc extract --author "Ruqiang ZOU" --v2 --dry-run
ppc extract --author "Ruqiang ZOU" --v2 --force
ppc extract --author "Ruqiang ZOU" --v2 --failed-only
ppc comprehend --author "Ruqiang ZOU" --v2 --dry-run
ppc comprehend --author "Ruqiang ZOU" --v2
ppc comprehend --author "Ruqiang ZOU" --v2 --profiled-only
ppc comprehend --author "Ruqiang ZOU" --v2 --force
ppc comprehend --author "Ruqiang ZOU" --v2 --author-profile --dry-run
ppc comprehend --author "Ruqiang ZOU" --v2 --author-profile
ppc comprehend --author "Ruqiang ZOU" --v2 --author-profile --deterministic
ppc profile --author "Ruqiang ZOU" --v2
ppc profile diff --author "Ruqiang ZOU"
ppc profile diff --author "Ruqiang ZOU" --markdown --output path/to/profile-diff-review.md
ppc profile signoff --input path/to/profile-diff-review.md
ppc profile signoff --input path/to/profile-diff-review.md --fail-on-hold
ppc profile gate --author "Ruqiang ZOU"
CHECK_PAPER_PROFILE_DIFF_REVIEW=path/to/profile-diff-review.md scripts/v2-default-readiness.sh "Ruqiang ZOU"
ppc ask --author "Ruqiang ZOU" --profile-version v2 "What are the author's main research themes?"
```

`classify` 会为已入库 chunks 写入确定性分类结果，包括 `chunk_kind`、`usefulness_score`、`skip_reason`、`classifier_version`、`source_hash` 和 `chunk_hash`。这些结果供后续 V2 chunk-level fact extraction 使用；默认 `ask` 和 Telegram 仍沿用 V1 profile + source chunk 问答链路。

`extract --v2` 会消费 current chunk classification，为 meaningful chunks 写入确定性 `chunk_facts`，包括 `claim_uid`、`fact_type`、`fact_json`、`confidence`、`extractor_version`、`source_hash` 和 `chunk_hash`。如果提示 `missing_current_classification` 大于 0，先运行 `ppc classify --author "Ruqiang ZOU"`；如果有失败记录，`--failed-only` 只重试这些 chunks。S2 仍不调用 LLM，也不会直接替换当前 `ask` 和 Telegram 默认问答链路。

`comprehend --v2` 会消费 current `chunk_facts`，聚合生成并保存 `PaperProfileV2`。S3 会保留每个 factual object 的 `chunk_fact_id`、`claim_uid` 和 source chunk evidence refs；LLM 只负责 synthesis 字段，不能伪造 evidence。没有 LLM 配置时会使用 deterministic profile builder；已有 LLM 配置但需要稳定复现时可加 `--deterministic`。`--profiled-only` 只构建已有 V1 paper profile 的论文，适合默认切换前先补齐 A/B 所需集合。`--dry-run` 会构建但不写库；`ppc profile diff --author "Ruqiang ZOU"` 用于对比 V1 paper profiles 与 V2 paper profiles，`--markdown --output` 可生成默认切换前的 profile diff review 文档。人工 review 后，用 `ppc profile signoff --input ... --fail-on-hold` 检查 `Human Signoff` 是否填写、每个 changed summary 是否已标为 `accepted` 或 `accepted_with_note`，避免在签核字段仍是 pending 时推进默认切换。

`comprehend --v2 --author-profile` 会消费已保存的 `PaperProfileV2`，按 theme-first 结构生成并保存 `AuthorProfileV2`。S4 会把 paper-level claims 合并为 `research_themes`、`research_evolution`、`methodological_strengths` 和 `representative_works`，并要求作者级 aggregate claims 继续带 `support_refs`，回指具体 `paper_key`、`claim_uid`、`chunk_fact_id` 和 `chunk_id`。如果 LLM synthesis 被 schema 或 evidence gate 拒绝，可用 `--deterministic` 生成稳定可复盘的作者级 V2 profile。`ppc profile --author "Ruqiang ZOU" --v2` 可查看并行 V2 作者画像；默认 `ask` 和 Telegram 仍不直接切换到 V2 作者画像。QA prompt `qa-v6` 会把 `answer_language` 固定为跟随用户问题；中文问题会要求 `answer`、`claims.claim`、`uncertainty` 和 `followup_queries` 用中文表达，即使 V2 profile summary 或 source chunks 是英文。

`ppc ask --profile-version v2` 会显式使用 `paper_profiles_v2` 和 `author_profiles_v2` 作为 profile context，用于在默认切换前做 V1/V2 A/B。`--profile-version auto` 会在目标作者已有 V2 paper profiles 时使用 V2，否则回退到 V1；不传 `--profile-version` 时读取 `CHECK_PAPER_QA_PROFILE_VERSION`，默认仍是 `v1`。如果显式使用 V2 但目标作者还没有 V2 profiles，命令会提示先运行 `ppc comprehend --author "AUTHOR" --v2`。QA 日志会记录实际使用的 `qa_profile_version`，`ppc logs qa` 会显示本次回答使用的是 V1 还是 V2 profile source。Telegram QA 也使用同一个配置项。

`ppc profile gate --author "Ruqiang ZOU"` 会检查 V2 paper profile 覆盖率、V2 schema/evidence 有效性、AuthorProfileV2 是否存在且有效，以及 support refs 数量，输出 `ready` 或 `blocked` 和具体 blockers/warnings。它用于决定 V2 profile-first QA 是否具备进入默认链路的前置条件；真正切换默认 QA 前仍应结合 `ppc eval` 的 `qa_mode_summary`、`qa_profile_version`、真实作者 baseline 和 profile diff 人工 signoff。`scripts/v2-default-readiness.sh` 会把当前目标 profile 默认值、`profile gate`、`profile signoff --fail-on-hold` 和 36 题 V1/V2 eval gate 汇总成一份 Markdown evidence；默认期望 `CHECK_PAPER_QA_PROFILE_VERSION=auto`，可用 `CHECK_PAPER_V2_TARGET_PROFILE_VERSION=v2` 改成 V2，设置 `CHECK_PAPER_PROFILE_DIFF_REVIEW` 后才会把人工签核纳入 gate。脚本不修改配置；失败项会让报告结果保持 `hold`，如需 CI/定时任务非零退出可加 `CHECK_PAPER_V2_READINESS_FAIL_ON_HOLD=1`。

分析和维护常用参数：

```bash
ppc analyze --author "Ruqiang ZOU" --failed-only
ppc analyze --author "Ruqiang ZOU" --stale-only
ppc analyze --author "Ruqiang ZOU" --force --skip-author-profile
ppc profile --author "Ruqiang ZOU"
ppc profile --author "Ruqiang ZOU" --v2
ppc profile --author "Ruqiang ZOU" --rebuild
```

任务、状态和日志：

```bash
ppc status --author "Ruqiang ZOU"
ppc backup
ppc jobs --author "Ruqiang ZOU" --status failed
ppc jobs --author "Ruqiang ZOU" --retry-failed
ppc jobs --cancel 123
ppc logs qa --author "Ruqiang ZOU" --last 20
ppc logs qa --errors
ppc logs qa --trend --days 14
ppc logs jobs --failed
ppc logs jobs --errors
ppc logs telegram --last 20
ppc logs telegram --summary
ppc logs telegram --with-qa --last 20
ppc logs telegram --trend --days 14
```

`ppc logs qa` 会显示每次问答的 `qa_mode`、`route_reason`、`delivery_mode`、`streaming_finalized`、stream delta 数、流式字符数、首个 delta 延迟和 stream 总耗时，用于复盘本次回答是走 `profile_first`、`source_evidence`、`hybrid` 还是 fallback，以及 Telegram 流式回答是否最终完成。
`ppc logs qa --trend` 会按天汇总 QA total/error、平均延迟、token/cost、streaming finalized 和 Telegram 关联量；`ppc logs telegram` 会显示 Telegram 平台投递侧记录，包括 preview edit attempts/successes/failures、最后预览字符数、最终投递状态、取消状态和投递错误码；`--summary` 会按最终投递状态和错误码聚合计数；`--with-qa` 会按 Telegram chat/job id 关联对应的 QA log，显示 author、question、qa_mode、route_reason、QA error 和 streaming finalized 状态；`--trend` 会按天汇总投递总量、取消/失败、edit/fallback、QA 关联数和 preview/reply 体量。

生产全量运行前先执行 `ppc backup`，它会在当前配置的数据库同目录生成带时间戳的 SQLite 备份；也可以用 `ppc backup --output /path/to/check_paper.backup.sqlite` 指定备份路径。
再执行 `ppc preflight --author "Ruqiang ZOU" --limit 5` 查看全量前检查清单，包括数据库路径、建议备份路径、LLM/Embedding/TG 配置状态、论文与队列统计、计划处理规模和失败恢复命令。

评测：

```bash
ppc eval --fixture tests/fixtures/golden_questions.json --top-k 8
ppc eval --fixture tests/fixtures/golden_questions.json --trace
ppc eval --fixture tests/fixtures/golden_questions.json --profile-version v2 --baseline-markdown
ppc eval --fixture tests/fixtures/golden_questions.json --compare-profile-versions --baseline-markdown
ppc eval --fixture data/eval/ruqiang_zou_golden_questions_expanded_2026-05-21.json --top-k 8 --compare-profile-versions --baseline-markdown
scripts/eval-v2-gate.sh
CHECK_PAPER_PROFILE_DIFF_REVIEW=path/to/profile-diff-review.md scripts/v2-default-readiness.sh "Ruqiang ZOU"
CHECK_PAPER_PROFILE_DIFF_REVIEW=path/to/profile-diff-review.md scripts/v2-default-switch-plan.sh "Ruqiang ZOU"
scripts/regression-check.sh
scripts/regression-deploy-evidence.sh
scripts/github-actions-evidence.sh
scripts/production-bootstrap-plan.sh launchd "Ruqiang ZOU"
scripts/production-readiness-evidence.sh "Ruqiang ZOU"
scripts/evidence-ledger.sh
scripts/log-trend-report.sh
scripts/regression-schedule-template.sh launchd
scripts/regression-schedule-template.sh systemd
scripts/regression-schedule-template.sh cron
ppc eval --fixture tests/fixtures/golden_questions.json --baseline-markdown \
  --output "/Users/hanlife02/Library/Mobile Documents/iCloud~md~obsidian/Documents/Ethan/2 - Docs/check-paper/check-paper 真实作者评测 baseline 2026-05-20.md"
```

评测报告会包含 `qa_profile_version` 和 `qa_mode_summary`，按 `profile_first`、`source_evidence` 等问答路由模式聚合 retrieval hit、citation precision 和 required-term 覆盖率。`--profile-version v2` 会用 V2 paper profile 覆盖率来规划 QA route，如果 fixture 作者还没有 V2 profiles，会提示先运行 `ppc comprehend --author "AUTHOR" --v2`。`--compare-profile-versions` 会在同一 fixture 下同时运行 V1/V2，并按默认切换阈值比较 `retrieval_hit_at_k`、`citation_precision` 和 `answer_contains_required`，同时要求 V2 候选结果满足绝对下限：默认 retrieval hit@k 1.000、citation precision 0.400、required-term 覆盖率 1.000；可用 `--min-candidate-*` 参数调整。比较报告会输出 `hold` 或 `eligible_for_manual_review`；加 `--fail-on-hold` 后会在报告写出后对 `hold` 返回非零退出码，适合把 36 题 strict baseline 固定为回归 gate。`scripts/eval-v2-gate.sh` 封装了这组默认 gate：默认读取 `data/eval/ruqiang_zou_golden_questions_expanded_2026-05-21.json`，也可传 fixture 路径或设置 `CHECK_PAPER_EVAL_FIXTURE`；设置 `CHECK_PAPER_EVAL_REPORT_DIR` 后会写 Markdown 报告。`scripts/v2-default-readiness.sh` 会进一步把目标机当前 `CHECK_PAPER_QA_PROFILE_VERSION`、V2 profile gate、profile diff signoff 和同一条 36 题 gate 合并为默认切换 readiness evidence，默认写到 `/private/tmp/check-paper-v2-readiness`；可用 `CHECK_PAPER_V2_AUTHOR`、`CHECK_PAPER_PROFILE_DIFF_REVIEW`、`CHECK_PAPER_V2_TARGET_PROFILE_VERSION`、`CHECK_PAPER_V2_READINESS_REPORT_DIR` 和 `CHECK_PAPER_V2_READINESS_FAIL_ON_HOLD=1` 调整。`scripts/v2-default-switch-plan.sh` 会只读运行当前 config、profile gate、profile signoff、目标 profile readiness dry run 和 evidence ledger snapshot，输出人工确认后的 apply/rollback checklist；默认写到 `/private/tmp/check-paper-v2-switch-plan`，不会改 `.paper-check.json` 或替人签核。`scripts/log-trend-report.sh` 会把 `ppc logs qa --trend`、`ppc logs telegram --trend` 和 Telegram summary 写成 Markdown，可用 `CHECK_PAPER_TREND_REPORT_DIR`、`CHECK_PAPER_TREND_DAYS`、`CHECK_PAPER_TREND_AUTHOR`、`CHECK_PAPER_TREND_CHAT_ID` 调整输出。`scripts/regression-check.sh` 是本地完整回归入口，会依次执行 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、`scripts/eval-v2-gate.sh` 和 `scripts/log-trend-report.sh`，默认把 gate、趋势报告和 `check-paper regression evidence <timestamp>.md` 写到 `/private/tmp/check-paper-eval-gate`，也可用 `CHECK_PAPER_REGRESSION_REPORT_DIR`、`CHECK_PAPER_EVAL_REPORT_DIR` 或 `CHECK_PAPER_TREND_REPORT_DIR` 改写。regression evidence 会记录本次运行结果、UTC 时间、git commit、fixture、top-k、失败步骤、产物路径和 `git status --short`，便于把本地或定时运行结果作为可追溯证据归档。`scripts/regression-schedule-template.sh` 会打印目标机器的 `launchd`、`systemd` timer 或 `cron` 模板，可用 `CHECK_PAPER_WORKDIR`、`CHECK_PAPER_REGRESSION_REPORT_DIR`、`CHECK_PAPER_REGRESSION_LOG`、`CHECK_PAPER_REGRESSION_WEEKDAY`、`CHECK_PAPER_REGRESSION_HOUR` 和 `CHECK_PAPER_REGRESSION_MINUTE` 调整路径和时间。`scripts/regression-deploy-evidence.sh` 会只读汇总目标机已加载的 launchd/systemd/cron regression 定时状态、最近 regression evidence 数量和最近 pass 记录，默认要求 14 天内至少 2 份 pass evidence；可用 `CHECK_PAPER_REGRESSION_DEPLOY_REPORT_DIR`、`CHECK_PAPER_REGRESSION_MIN_PASS_COUNT`、`CHECK_PAPER_REGRESSION_MAX_EVIDENCE_AGE_DAYS` 和 `CHECK_PAPER_REGRESSION_DEPLOY_FAIL_ON_HOLD=1` 调整。`scripts/github-actions-evidence.sh` 会只读检查本地 regression workflow 形状，并在 `gh` 已安装且认证时汇总最近远端 workflow run，默认要求最近成功数不少于 2；它不会触发 workflow、下载 artifact 或修改 GitHub 设置。`scripts/production-bootstrap-plan.sh launchd|systemd|cron` 会只读生成目标机 bootstrap plan 和模板包，包含 Telegram service/logrotate、regression scheduler、Telegram logrotate schedule、apply checklist 和 verification commands；默认写到 `/private/tmp/check-paper-production-bootstrap`，可用 `CHECK_PAPER_PRODUCTION_BOOTSTRAP_REPORT_DIR` 调整输出。`scripts/production-readiness-evidence.sh` 会只读串起 `scripts/v2-default-readiness.sh`、`scripts/regression-deploy-evidence.sh` 和 `scripts/telegram-deploy-evidence.sh`，把三份子报告汇总成目标机总验收 Markdown，默认写到 `/private/tmp/check-paper-production-readiness`；可用 `CHECK_PAPER_PRODUCTION_READINESS_REPORT_DIR` 调整输出，只有三份子报告都为 `ready` 时才输出总 `ready`，如需总报告 `hold` 时返回非零退出可设置 `CHECK_PAPER_PRODUCTION_READINESS_FAIL_ON_HOLD=1`。`scripts/evidence-ledger.sh` 会只读扫描 eval gate、trend、regression evidence、GitHub Actions evidence、V2 switch plan/readiness、deploy evidence、production bootstrap/readiness Markdown，输出连续证据台账；默认写到 `/private/tmp/check-paper-evidence-ledger`，可用 `CHECK_PAPER_EVIDENCE_LEDGER_SCAN_DIRS` 和 `CHECK_PAPER_EVIDENCE_LEDGER_RECENT` 调整扫描范围和最近条数。`.github/workflows/regression.yml` 会在 push、pull request、每周定时和手动触发时运行同一条 regression gate，并上传 eval gate、趋势 Markdown 和 regression evidence artifact；CI 中会把报告目录设置到 runner temp，避免依赖本机 `/private/tmp`。仓库内的 Ruqiang ZOU 扩展 fixture 当前覆盖 36 个问题和 14 个 V2 paper profiles，并加入跨论文/主题题、数值题、实验条件、方法细节题和三篇/多篇概览题；当前更严格的 36 题集已通过候选绝对质量 gate，输出 `eligible_for_manual_review`，但不会自动改变默认 V1 QA 行为。`--trace` 会额外输出各检索 route 的候选、rank、RRF score，以及最终 fusion 排名。`--baseline-markdown` 会输出适合放进 Obsidian 的 baseline 或比较报告；配合 `--output` 可直接写入指定路径。

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

如果没有在命令中指定作者，bot 会优先使用当前 chat 通过 `/use_author` 或 `/authors` 设置的作者；该设置会写入 SQLite，`ppc serve-telegram` 重启后仍然生效。未设置时使用 `CHECK_PAPER_DEFAULT_AUTHOR`。私聊中设置默认作者后，可以直接发送问题。

群聊中需要艾特 bot 才会响应，例如：

```text
@你的Bot用户名 这篇论文讲什么？
/ask@你的Bot用户名 这个人的 MOF 相关成果有哪些？
```

只发送 `@你的Bot用户名` 不带内容时，等价于 `/help`。群聊中的裸 `/status`、`/help`、`/ask` 不会响应，必须带命令 mention 或在消息中显式 @ bot。群聊里的 admin-only 命令，例如 `/sync`、`/analyze`、`/embed`、`/comprehend` 和 rebuild 类命令，只有 `TELEGRAM_ADMIN_USER_IDS` 中的用户会被放行；当前 Telegram 端未实现的 slash 命令会直接返回未知命令，不会落入普通 QA。

问答类消息会先回复 `处理中...`，随后通过 Telegram `editMessageText` 流式更新 answer 预览；最终通过本地 schema 和 evidence 校验后，再替换成正式的对话式回答。流式问答会在 `qa_logs.delivery_mode=streaming` 下记录 `streaming_finalized=true|false`、`stream_delta_count`、`streamed_chars`、`stream_first_delta_ms` 和 `stream_duration_ms`，便于复盘预览已发出但最终校验或 stream API 失败的场景。Telegram 服务日志和 `telegram_delivery_logs` 表还会记录 preview edit attempts/successes/failures、最后预览字符数，以及最终投递状态 `edited_placeholder`、`sent_fallback`、`empty`、`skipped_cancelled` 或 `failed`；如果用户取消任务，结构化日志会记录 `cancelled=true`。`/help`、`/status`、`/authors`、`/jobs`、`/sources` 等轻量命令不走流式。

如果配置了 `TELEGRAM_CHAT_IDS`，bot 只会响应这些私聊或群聊；群聊 chat id 通常是负数。`TELEGRAM_ADMIN_USER_IDS` 使用 Telegram user id，不是 chat id。

## 数据位置

默认数据库：

```text
data/check_paper.sqlite
```

数据库里保存：

- 论文元数据、source hash、parser version 和 cleaner version；当前 `source-cleaner-v13` 会按 publisher/source 分层去除 ScienceDirect/Elsevier、Wiley、MDPI/PLOS、ACS、RSC、Springer/Nature、Frontiers、Taylor & Francis、IEEE、Oxford Academic/OUP、SAGE、Cell Press、PNAS、eLife、AAAS/Science、IOPscience、AIP Publishing 等常见页面导航、访问入口、指标和页脚噪声
- 清洗后的正文 chunk，以及 figure/table caption 的 `section_kind`、`caption_label`、`caption_object_type`、`caption_object_label`、`caption_panel_labels_json`、`caption_target_labels_json`、`caption_panel_details_json`、`caption_measurements_json`、`caption_conditions_json` 和 `caption_values_json` 元数据；`markdown-parser-v4` 会保留常见面板/范围 label，例如 `Fig. S1a,b`、`Figure 2A-C` 和 `Table S2-S4`，并结构化为面板列表和目标图表对象列表；caption 内部数值/条件/measurement 会用 deterministic 规则抽取后随 chunk 入库并进入 QA/eval trace；`caption_panel_details_json` 还会在 panel 局部描述中记录基础跨 panel 关系，例如 `A shows higher conversion than B`、`A induces B`、`B suppresses C`，并把相邻关系边保留为 `relation_paths` 两跳链路，例如 `A causes B; B inhibits C`；同时用 `cross_references` 记录按引用局部上下文判断 `relation` 的外部图表目标，例如 `derived_from Fig. 2B`、`compared_with Table S1`、`summarized_in Table S2`、`caused_by Fig. 8A`、`inhibited_by Table S3`
- FTS 检索索引
- chunk embedding 和 embedding 版本信息
- 每篇论文的 LLM 理解 JSON
- V2 chunk classification 和 chunk facts
- V2 paper profiles
- V2 author profiles
- 作者级聚合画像 JSON
- 分析任务队列、任务状态历史和失败原因
- QA 日志、引用快照、token 用量和估算成本
- Telegram preview/final delivery 结构化投递日志
