import os
import jwt
import secrets
import datetime
from flask import request, jsonify, abort

# 动态生成随机密钥
JWT_SECRET = os.environ.get("JWT_SECRET", secrets.token_hex(32))  # 32字节的随机密钥
JWT_ALGORITHM = "HS256"

# 从环境变量获取用户名和密码
USERNAME = os.environ.get("AUTH_USERNAME", "admin")
PASSWORD = os.environ.get("AUTH_PASSWORD", "password")


def generate_token(username):
    """
    生成JWT Token
    :param username: 用户名
    :return: JWT Token
    """
    payload = {
        'username': username,
        'exp': datetime.datetime.utcnow() + datetime.timedelta(hours=2)  # Token有效期2小时
    }
    token = jwt.encode(payload, JWT_SECRET, algorithm=JWT_ALGORITHM)
    return token


def authenticate_request():
    """
    鉴权中间件，用于验证JWT Token
    """
    excluded_routes = ['/api/login', '/']  # 排除无需鉴权的路由
    if request.path in excluded_routes:
        return
    if request.path.startswith('/static/'):
        return
        
    # 首先检查cookie中是否有token
    token_from_cookie = request.cookies.get('auth_token')
    if token_from_cookie:
        try:
            jwt.decode(token_from_cookie, JWT_SECRET, algorithms=[JWT_ALGORITHM])
            return
        except (jwt.ExpiredSignatureError, jwt.InvalidTokenError):
            pass  # 如果cookie中的token无效，继续检查Authorization头
    
    # 检查Authorization头
    auth_header = request.headers.get('Authorization')
    if not auth_header or not auth_header.startswith("Bearer "):
        abort(401, description="未提供有效的认证令牌")

    token = auth_header.split(" ")[1]
    try:
        jwt.decode(token, JWT_SECRET, algorithms=[JWT_ALGORITHM])
    except jwt.ExpiredSignatureError:
        abort(401, description="令牌已过期")
    except jwt.InvalidTokenError:
        abort(401, description="无效的令牌")


def login():
    """
    用户登录接口，返回JWT Token
    :return: JSON响应
    """
    data = request.json
    if not data or 'username' not in data or 'password' not in data:
        return jsonify({'success': False, 'message': '用户名和密码是必填项'}), 400

    username = data['username']
    password = data['password']

    if username == USERNAME and password == PASSWORD:
        token = generate_token(username)
        return jsonify({'success': True, 'token': token})

    return jsonify({'success': False, 'message': '用户名或密码错误'}), 401