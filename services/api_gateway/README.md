# Flowwisper API Gateway

FastAPI 脚手架，负责桌面端与云端服务的统一入口、鉴权、速率限制与会话编排。默认提供健康检查与 Session 路由占位接口。

## 开发指南

```bash
cd services/api_gateway
uvicorn flowwisper_api.main:app --reload
```

后续需要：

- 接入 OAuth2/JWT 中间件与多租户上下文。
- 调用 Hybrid Router、Sync Service 与通知系统。
- 补充 API 文档与 OpenAPI 扩展描述。
