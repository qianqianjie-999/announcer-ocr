# -*- coding: utf-8 -*-
"""calibrate.py — 一次性配置工具：框选识别区域(ROI)写入 config.json
用法: python calibrate.py
操作: 弹出摄像头画面 -> 鼠标拖框选中屏幕上排"当前办理"文字区域(避开滚动字幕行)
      -> 按 Enter/空格 确认，ESC 取消。框选结果自动写入 config.json 的 roi 字段。
"""
import json
import cv2

CFG = "config.json"

def main():
    cfg = json.load(open(CFG, encoding="utf-8"))
    src = cfg.get("camera_source", cfg.get("camera_index", 0))
    if isinstance(src, str) and src.isdigit():
        src = int(src)
    cap = cv2.VideoCapture(src)
    if isinstance(src, int) and cfg.get("capture_width"):
        cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg["capture_width"])
        cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg["capture_height"])
    ok, frame = cap.read()
    cap.release()
    if not ok:
        raise SystemExit(f"取流失败({src})，请检查 USB 索引或 RTSP 地址/账号")

    print("请拖框选中屏幕上排文字区域（避开下方滚动字幕行），Enter 确认")
    x, y, w, h = cv2.selectROI("拖框选ROI后按Enter", frame, showCrosshair=True)
    cv2.destroyAllWindows()
    if w == 0 or h == 0:
        raise SystemExit("未选择区域，取消")
    cfg["roi"] = [int(x), int(y), int(w), int(h)]
    json.dump(cfg, open(CFG, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    print(f"已保存 roi={cfg['roi']} 到 {CFG}")

if __name__ == "__main__":
    main()
