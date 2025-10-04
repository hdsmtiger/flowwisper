# Hybrid Router Service

用于在本地与云端语音/LLM 引擎之间进行会话级决策的 Go 服务脚手架。当前仅提供健康检查与占位决策接口。

```bash
cd services/hybrid_router
go run main.go
```

后续任务：

- 引入引擎性能指标缓存与熔断策略。
- 与 API Gateway、Core Service 建立 gRPC/WebSocket 通道。
- 上报 Prometheus 指标与 OpenTelemetry Trace。
