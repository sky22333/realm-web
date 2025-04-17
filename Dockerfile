FROM python:3.8-alpine

# 安装必要的系统工具和iptables
RUN apk update && \
    apk add --no-cache iptables net-tools && \
    rm -rf /var/cache/apk/*

COPY . .

# 安装依赖
RUN pip install --no-cache-dir -r requirements.txt && \
    mkdir -p /app/data

# 暴露端口
EXPOSE 888

# 启动
CMD ["python3", "app.py"]