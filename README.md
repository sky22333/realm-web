### 基于`iptables`的批量管理端口转发面板

练手学习的小项目，`python`语言。

#### `Docker`部署
> 请修改环境变量用户名和密码
```
docker run -d \
  --name iptables-web \
  --privileged \
  --network host \
  --restart always \
  -e AUTH_USERNAME=admin123 \
  -e AUTH_PASSWORD=admin123 \
  -v ./data:/app/data \
  ghcr.io/sky22333/iptables-web
```
