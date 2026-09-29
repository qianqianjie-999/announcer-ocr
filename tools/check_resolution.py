#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
check_resolution.py —— 摄像头原生分辨率与"软件放大"体检

目的：判断画面模糊是不是「驱动把小分辨率插值放大成大图」造成的。
做法：同一场景下依次以多个分辨率取帧，比较：
      ① 驱动实际返回的尺寸（若设1920x1080却还是小图，说明不支持）
      ② 各帧的细节量（锐度）
      ③ 把小图的放大版与直接抓的大图对比（若几乎一样 = 驱动在放大充数）
输出：res_<W>x<H>.jpg 各分辨率原图，以及 res_compare.jpg 对比拼图
"""
import cv2
import numpy as np

RES_LIST = [(640, 480), (1280, 720), (1920, 1080)]


def grab(w, h):
    cap = cv2.VideoCapture(0)
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, w)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, h)
    cap.set(cv2.CAP_PROP_BUFFERSIZE, 1)
    for _ in range(15):
        cap.read()
    ok, f = cap.read()
    cap.release()
    return f if ok else None


def sharpness(img):
    g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    return cv2.Laplacian(g, cv2.CV_64F).var()


def main():
    frames = {}
    print("依次以各分辨率取帧（每次重新打开设备）...")
    for w, h in RES_LIST:
        f = grab(w, h)
        if f is None:
            print(f"  {w}x{h}: 取帧失败")
            continue
        ah, aw = f.shape[:2]
        frames[(w, h)] = f
        cv2.imwrite(f"res_{w}x{h}.jpg", f)
        print(f"  请求 {w}x{h} -> 实际得到 {aw}x{ah}   锐度 {sharpness(f):6.1f}"
              + ("   <-- 驱动没给到该尺寸" if (aw, ah) != (w, h) else ""))

    if not frames:
        raise SystemExit("无可用帧")

    # 统一尺寸(1280x720)拼接对比, 并在统一尺寸下重算锐度(公平比较)
    TW, TH = 1280, 720
    tiles = []
    print("\n统一缩放到 1280x720 后比较（这才是公平的清晰度对比）：")
    for (w, h), f in sorted(frames.items()):
        t = cv2.resize(f, (TW, TH), interpolation=cv2.INTER_AREA if f.shape[1] > TW else cv2.INTER_LANCZOS4)
        s = sharpness(t)
        print(f"  原始{w}x{h} -> 统一尺寸下锐度 {s:6.1f}")
        cv2.putText(t, f"src {w}x{h}  sharp {s:.0f}", (14, 40),
                    cv2.FONT_HERSHEY_SIMPLEX, 1.0, (0, 255, 255), 2)
        tiles.append(t)

    cv2.imwrite("res_compare.jpg", np.vstack(tiles))

    # 关键判据: 用小图放大后与大图对比, 看大图是否只是"放大的小图"
    if (640, 480) in frames and (1920, 1080) in frames:
        small = frames[(640, 480)]
        big = frames[(1920, 1080)]
        up = cv2.resize(small, (big.shape[1], big.shape[0]), interpolation=cv2.INTER_LANCZOS4)
        g1 = cv2.cvtColor(up, cv2.COLOR_BGR2GRAY).astype(np.float32)
        g2 = cv2.cvtColor(big, cv2.COLOR_BGR2GRAY).astype(np.float32)
        mse = float(np.mean((g1 - g2) ** 2))
        psnr = 10 * np.log10(255.0 ** 2 / mse) if mse > 0 else 99.0
        print(f"\n【关键判据】480p 放大到 1080p 与直接抓的 1080p 对比：PSNR = {psnr:.1f} dB")
        print("   PSNR > 32 dB  → 两者几乎一样，说明 1080p 就是小图插值放大的（糊的根源）")
        print("   PSNR < 28 dB  → 大图有真实额外的细节，摄像头原生支持高分辨率")
        cv2.imwrite("res_up_vs_native.jpg", np.hstack([up, big]))

    print("\n对比拼图已存: res_compare.jpg" + (", res_up_vs_native.jpg" if len(frames) > 1 else ""))


if __name__ == "__main__":
    main()
