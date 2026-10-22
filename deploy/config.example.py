# -*- coding: utf-8 -*-
"""本地部署配置模板。

用法：
    1. 复制本文件为 deploy/config.local.py
    2. 按现场实际情况填写
    3. config.local.py 已被 .gitignore 排除，不会进版本库

说明：本文件为**现场部署工装**（库外远程上传/重启类脚本）的配置模板——
      仓库内脚本当前不消费 config.local.py；TIER/USE_SUBSTREAM 供工装使用。
      缺失时工装回退到命令行参数或交互输入，不写死任何现场地址。
      ★2026-10-23 澄清：若你只用库内脚本，本文件可不配。
"""

# ---- 目标主机（现场小主机的内网地址）----
HOST = "192.168.8.100"          # 例：改为现场实际地址
PORT = 22
USER = "debian"                 # 目标机上运行服务的账号
PASSWORD = ""                   # 留空则用 SSH 密钥；不建议明文长期保存

# ---- 部署路径 ----
REMOTE_DIR = "/home/debian/anntest"   # 程序与配置的落地目录
SERVICE_NAME = "announce-rs"          # systemd 单元名（Python 版为 announce）

# ---- 摄像头（RTSP 场景）----
# 密码含 @ : # ? 等符号需 URL 转义（@ → %40）
RTSP_URL = ""                   # 例：rtsp://admin:pass@192.168.8.64:554/h264/ch1/sub/av_stream
USE_SUBSTREAM = True            # 弱机档务必用子码流（720P/D1@5-10fps）

# ---- 性能档位：'weak'(J1850 类 2 核低频) / 'strong'(4 核+ 或高频 x86) ----
TIER = "weak"
