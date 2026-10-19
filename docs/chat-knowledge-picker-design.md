# 对话本地知识：按需查找

用户不需要引用、勾选或绑定知识库。已移除对话选择组件与任务配置中的知识库勾选项。

- 原生执行、计划模式：自动发现可读的本地知识库，将 `knowledge_search` 作为只读能力注册到现有 Agent 循环，由助手根据问题按需检索。普通消息不预先添加知识片段。
- Claude Code、Pi、OpenCode 执行模式：通过同一个本机工具桥接按需调用 `dandelion_knowledge_search`，沿用原生检索与资料权限；关闭 Wiki 写入不会关闭该只读能力。
- 问答模式：不开放工具调用，核心按问题检索，只有相关命中才提供依据，未命中保持普通对话。
- 自动发现覆盖所有可读取且未归档的本地库，不按旧的“最多勾选 4 个”截断。工具调用时重新发现，导入后的资料可以在既有对话中找到。
- 保留来源链接、撤回与删除即时失效、草稿不参与检索、外发权限等已有规则。不合并或改名任何知识库。

延用共享检索索引和单一 Agent 循环；不新增循环或事件协议。改动前核对了本机可用源码位置，并阅读上游 Hermes 和 Codex 的工具执行/注册实现：
https://github.com/NousResearch/hermes-agent/blob/main/run_agent.py
https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/registry.rs
