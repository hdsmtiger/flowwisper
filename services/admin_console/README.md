# Flowwisper Admin Console

基于 Next.js 的管理后台脚手架，为企业租户提供策略配置、审计与密钥管理功能的基础结构。当前页面展示开发路线与占位 UI。

```bash
cd services/admin_console
npm install
npm run dev
```

后续计划：

- 集成企业租户登录、RBAC 与策略下发 API。
- 提供多环境部署配置（预发/生产）。
- 对接 Sync Service 与日志检索服务，完成数据面集成。
