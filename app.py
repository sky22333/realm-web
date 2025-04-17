from flask import Flask, request, jsonify, abort, redirect, send_from_directory
import subprocess
import os
import socket
import json
from contextlib import closing
from auth import login, authenticate_request

# 明确指定静态文件夹路径
app = Flask(__name__, static_folder='static', static_url_path='/static')
# 注册登录路由
app.route('/api/login', methods=['POST'])(login)
# 注册鉴权中间件
app.before_request(authenticate_request)

class IPTablesManager:
    def __init__(self):
        self.rules = {}  # 存储当前的转发规则
        self.default_start_port = 1000  # 设置默认起始端口
        # 默认保留的系统端口
        self.reserved_ports = {22, 80, 53, 21, 25, 23, 110, 143, 888}
        # 规则文件路径
        self.rules_file = '/app/data/iptables_rules.json'
        # 加载保存的规则
        self.load_rules()

    def load_rules(self):
        """从文件加载保存的规则"""
        try:
            if os.path.exists(self.rules_file):
                with open(self.rules_file, 'r') as f:
                    saved_rules = json.load(f)
                # 清空现有规则
                self.clear_all_iptables_rules()
                # 重新应用已保存的规则
                for port, rule in saved_rules.items():
                    self.add_rule(
                        rule['local_port'],
                        rule['target_ip'],
                        rule['target_port']
                    )
        except Exception as e:
            print(f"Error loading rules: {str(e)}")
            self.rules = {}

    def save_rules(self):
        """保存规则到文件"""
        try:
            with open(self.rules_file, 'w') as f:
                json.dump(self.rules, f, indent=2)
        except Exception as e:
            print(f"Error saving rules: {str(e)}")

    def clear_all_iptables_rules(self):
        """清除所有 iptables 转发规则"""
        try:
            # 清除 nat 表的 PREROUTING 链
            subprocess.run(['iptables', '-t', 'nat', '-F', 'PREROUTING'])
            # 清除 nat 表的 POSTROUTING 链
            subprocess.run(['iptables', '-t', 'nat', '-F', 'POSTROUTING'])
            # 清除 filter 表的 FORWARD 链
            subprocess.run(['iptables', '-F', 'FORWARD'])
        except subprocess.CalledProcessError as e:
            print(f"Error clearing iptables rules: {str(e)}")

    def is_port_open(self, port):
        """检查端口是否可用（未被任何服务占用）"""
        try:
            # 检查 TCP 端口
            with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as sock:
                if sock.connect_ex(('127.0.0.1', port)) == 0:
                    return False  # 端口被占用
            
            # 检查是否是保留端口
            if port in self.reserved_ports:
                return False

            # 检查是否被 iptables 规则占用
            return not self.is_port_in_use_iptables(port)
        except:
            return False

    def is_port_in_use_iptables(self, port):
        """检查端口是否被 iptables 规则占用"""
        return str(port) in self.rules

    def is_port_in_use(self, port):
        """综合检查端口是否被占用"""
        return not self.is_port_open(port)

    def find_next_available_port(self, start_port):
        """查找下一个可用端口"""
        current_port = start_port
        while self.is_port_in_use(current_port):
            current_port += 1
            if current_port > 65535:  # 确保不超过最大端口号
                raise ValueError("No available ports found")
        return current_port

    def add_rule(self, local_port, target_ip, target_port):
        """添加iptables转发规则"""
        try:
            # 添加PREROUTING规则进行端口转发
            subprocess.run([
                'iptables', '-t', 'nat', '-A', 'PREROUTING',
                '-p', 'tcp', '--dport', str(local_port),
                '-j', 'DNAT', '--to-destination', f'{target_ip}:{target_port}'
            ], check=True)
            
            # 添加POSTROUTING规则进行源地址转换
            subprocess.run([
                'iptables', '-t', 'nat', '-A', 'POSTROUTING',
                '-p', 'tcp', '-d', target_ip, '--dport', str(target_port),
                '-j', 'MASQUERADE'
            ], check=True)
            
            # 添加FORWARD规则允许转发
            subprocess.run([
                'iptables', '-A', 'FORWARD',
                '-p', 'tcp', '-d', target_ip, '--dport', str(target_port),
                '-j', 'ACCEPT'
            ], check=True)
            
            subprocess.run([
                'iptables', '-A', 'FORWARD',
                '-p', 'tcp', '-s', target_ip, '--sport', str(target_port),
                '-j', 'ACCEPT'
            ], check=True)
            
            # 保存规则到内存和文件
            self.rules[str(local_port)] = {
                'local_port': local_port,
                'target_ip': target_ip,
                'target_port': target_port
            }
            self.save_rules()
            return True
        except subprocess.CalledProcessError:
            return False

    def delete_rule(self, local_port):
        """删除指定端口的转发规则"""
        if str(local_port) not in self.rules:
            return False
        
        rule = self.rules[str(local_port)]
        try:
            # 删除PREROUTING规则
            subprocess.run([
                'iptables', '-t', 'nat', '-D', 'PREROUTING',
                '-p', 'tcp', '--dport', str(local_port),
                '-j', 'DNAT', '--to-destination', 
                f'{rule["target_ip"]}:{rule["target_port"]}'
            ], check=True)
            
            # 删除POSTROUTING规则
            subprocess.run([
                'iptables', '-t', 'nat', '-D', 'POSTROUTING',
                '-p', 'tcp', '-d', rule["target_ip"], '--dport', str(rule["target_port"]),
                '-j', 'MASQUERADE'
            ], check=True)
            
            # 删除FORWARD规则
            subprocess.run([
                'iptables', '-D', 'FORWARD',
                '-p', 'tcp', '-d', rule["target_ip"], '--dport', str(rule["target_port"]),
                '-j', 'ACCEPT'
            ], check=True)
            
            subprocess.run([
                'iptables', '-D', 'FORWARD',
                '-p', 'tcp', '-s', rule["target_ip"], '--sport', str(rule["target_port"]),
                '-j', 'ACCEPT'
            ], check=True)
            
            # 从存储中删除规则并保存
            del self.rules[str(local_port)]
            self.save_rules()
            return True
        except subprocess.CalledProcessError:
            return False

    def get_all_rules(self):
        """获取所有转发规则"""
        return list(self.rules.values())

    def get_system_used_ports(self):
        """获取系统当前使用的所有端口"""
        used_ports = set()
        
        try:
            # 检查 TCP 端口
            output = subprocess.check_output(['netstat', '-tln']).decode()
            for line in output.split('\n')[2:]:  # 跳过头部
                if line.strip():
                    parts = line.split()
                    if len(parts) >= 4:
                        addr = parts[3]
                        if ':' in addr:
                            port = addr.split(':')[-1]
                            if port.isdigit():
                                used_ports.add(int(port))
        except:
            pass
        
        # 添加已知的保留端口
        used_ports.update(self.reserved_ports)
        
        # 添加已经被 iptables 规则使用的端口
        used_ports.update(int(port) for port in self.rules.keys())
        
        return sorted(list(used_ports))

# 创建IPTablesManager实例
iptables_manager = IPTablesManager()

@app.route('/api/rules', methods=['GET'])
def get_rules():
    """获取所有转发规则"""
    return jsonify(iptables_manager.get_all_rules())

@app.route('/api/rules', methods=['POST'])
def add_rules():
    """添加转发规则"""
    try:
        data = request.json
        mode = data.get('mode', 'auto')
        ip_list = data.get('ip_list', '').strip().split('\n')
        port_data = data.get('port_data', {})
        
        # 验证输入
        if not ip_list or not ip_list[0]:
            return jsonify({'success': False, 'message': '请输入落地IP和端口列表'})

        # 根据不同模式处理端口分配
        if mode == 'specific':
            # 指定起始端口自动分配模式
            start_port = port_data.get('startPort')
            if not start_port:
                return jsonify({'success': False, 'message': '请输入起始端口'})
            try:
                current_port = int(start_port)
                if current_port < 1 or current_port > 65535:
                    return jsonify({'success': False, 'message': '起始端口必须在1-65535之间'})
            except ValueError:
                return jsonify({'success': False, 'message': '无效的起始端口'})
            
            # 从起始端口开始自动分配
            current_ports = []
            for _ in ip_list:
                try:
                    port = iptables_manager.find_next_available_port(current_port)
                    current_ports.append(port)
                    current_port = port + 1
                except ValueError:
                    return jsonify({'success': False, 'message': '无法找到可用端口'})
                
        elif mode == 'manual':
            # 手动指定端口模式
            ports = port_data.get('ports', [])
            if len(ports) != len(ip_list):
                return jsonify({'success': False, 'message': '指定端口数量与规则数量不匹配'})
            
            # 检查端口格式和占用情况
            occupied_ports = []
            for port in ports:
                try:
                    port_num = int(port)
                    if port_num < 1 or port_num > 65535:
                        return jsonify({'success': False, 'message': f'端口 {port} 超出范围(1-65535)'})
                    if iptables_manager.is_port_in_use(port_num):
                        occupied_ports.append(port)
                except ValueError:
                    return jsonify({'success': False, 'message': f'无效的端口号：{port}'})
            
            if occupied_ports:
                return jsonify({
                    'success': False,
                    'message': '部分端口已被占用',
                    'occupied_ports': occupied_ports
                })
            
            current_ports = [int(p) for p in ports]
        else:
            # 完全自动分配模式
            current_port = iptables_manager.default_start_port
            current_ports = []
            for _ in ip_list:
                try:
                    port = iptables_manager.find_next_available_port(current_port)
                    current_ports.append(port)
                    current_port = port + 1
                except ValueError:
                    return jsonify({'success': False, 'message': '无法找到可用端口'})

        # 添加规则
        success = True
        added_rules = []
        
        for i, line in enumerate(ip_list):
            line = line.strip()
            if not line:
                continue
                
            try:
                if ':' not in line:
                    return jsonify({'success': False, 'message': f'无效的格式（需要IP:端口）：{line}'})
                
                target_ip, target_port_str = line.split(':')
                target_port = int(target_port_str)
                local_port = current_ports[i]
                
                if not iptables_manager.add_rule(local_port, target_ip, target_port):
                    success = False
                    break
                
                added_rules.append({
                    'local_port': local_port,
                    'target_ip': target_ip,
                    'target_port': target_port
                })
                
            except Exception as e:
                return jsonify({'success': False, 'message': f'处理规则时出错：{str(e)}'})
        
        if success:
            return jsonify({
                'success': True,
                'message': '添加成功',
                'added_rules': added_rules
            })
        else:
            # 如果添加失败，回滚已添加的规则
            for rule in added_rules:
                iptables_manager.delete_rule(rule['local_port'])
            return jsonify({'success': False, 'message': '添加规则失败，已回滚所有更改'})
            
    except Exception as e:
        return jsonify({'success': False, 'message': f'系统错误：{str(e)}'})

@app.route('/api/rules/<int:port>', methods=['DELETE'])
def delete_rule(port):
    """删除单个转发规则"""
    success = iptables_manager.delete_rule(port)
    return jsonify({'success': success})

@app.route('/api/rules/batch', methods=['DELETE'])
def delete_batch_rules():
    """批量删除转发规则"""
    ports = request.json.get('ports', [])
    success = True
    deleted_ports = []
    failed_ports = []

    # 批量删除规则
    for port in ports:
        try:
            port = int(port)
            if iptables_manager.delete_rule(port):
                deleted_ports.append(port)
            else:
                success = False
                failed_ports.append(port)
        except (ValueError, TypeError):
            success = False
            failed_ports.append(port)

    return jsonify({
        'success': success,
        'deleted_ports': deleted_ports,
        'failed_ports': failed_ports
    })



@app.route('/api/ports/used', methods=['GET'])
def get_used_ports():
    """获取所有已使用的端口"""
    return jsonify(iptables_manager.get_system_used_ports())

@app.route('/', methods=['GET', 'POST'])
def index():
    # 检查用户是否已登录（通过鉴权中间件）
    auth_header = request.headers.get('Authorization')
    
    # 如果是POST请求，检查是否有token参数
    if request.method == 'POST' and request.form.get('token'):
        token = request.form.get('token')
        # 设置cookie并重定向到GET请求
        response = redirect('/')
        response.set_cookie('auth_token', token, httponly=True)
        return response
    
    # 检查cookie中是否有token
    token_from_cookie = request.cookies.get('auth_token')
    if token_from_cookie:
        # 如果cookie中有token，则视为已登录
        return send_from_directory(app.static_folder, 'index.html')
    
    # 检查Authorization头
    if auth_header and auth_header.startswith("Bearer "):
        return send_from_directory(app.static_folder, 'index.html')
    
    # 未登录，重定向到登录页面
    return redirect('/static/login.html')

if __name__ == '__main__':

    # 确保以root权限运行
    if os.geteuid() != 0:
        print("Error: This application must be run with root privileges")
        exit(1)
    
    # 确保数据目录存在
    os.makedirs('/app/data', exist_ok=True)
    
    # 启用IP转发
    try:
        with open('/proc/sys/net/ipv4/ip_forward', 'w') as f:
            f.write('1')
    except Exception as e:
        print(f"Warning: Could not enable IP forwarding: {str(e)}")
    app.run(host='0.0.0.0', port=888)