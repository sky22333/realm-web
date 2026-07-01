# Realm 转发面板

基于 [realm](https://github.com/zhboner/realm) 的 TCP/UDP 端口转发管理面板。转发内核嵌入 `realm_core`（DNS + realm_io），前端构建产物嵌入单一二进制。

## 功能

- 批量添加转发规则（自动 / 指定起始 / 手动端口）
- 每条规则同时转发 **TCP + UDP**（realm_core 转发栈）
- 端口级流量统计（TCP 本地侧计量 + UDP 报文计量）与 SQLite 持久化
- JWT 登录保护
- 响应式中文界面（shadcn-vue 风格）

## 快速开始

### 环境要求

- Rust 1.85+（edition 2024）
- Node.js 20+

### 本地开发

```bash
# 安装前端依赖并构建（cargo build 会自动触发，也可手动）
cd frontend && npm install && npm run build && cd ..

# 编译并运行
cd backend
cargo run
```

首次启动时，若未设置 `AUTH_USERNAME` 与 `AUTH_PASSWORD`，程序会在终端随机生成账号密码（小写字母 + 数字）并打印，请妥善保存。

面板地址：http://127.0.0.1:888

### Docker

```bash
docker compose up -d --build
```

镜像基于 Alpine，BuildKit 会按当前主机架构构建（`linux/amd64` 或 `linux/arm64`）。多架构推送见 `.github/workflows/docker-ghcr.yml`。

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PANEL_PORT` | `888` | 管理面板端口 |
| `DATA_DIR` | `./data` | 数据目录（SQLite） |
| `AUTH_USERNAME` | - | 登录用户名；须与 `AUTH_PASSWORD` 同时设置 |
| `AUTH_PASSWORD` | - | 登录密码；须与 `AUTH_USERNAME` 同时设置 |
| `JWT_SECRET` | 每次启动随机 | 可选覆盖；仅内存，重启后需重新登录 |
| `DEFAULT_START_PORT` | `1000` | 自动分配起始端口 |
| `RESERVED_PORTS` | `22,80,443,888` | 保留端口 |
| `SKIP_WEB_BUILD` | - | 设为 `1` 跳过 build.rs 中的前端构建 |

## 项目结构

```
backend/          Rust 单体（API + realm_core 转发 + 嵌入前端）
frontend/         Vue 3 源码（构建后嵌入二进制）
```
