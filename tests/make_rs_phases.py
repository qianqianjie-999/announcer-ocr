# -*- coding: utf-8 -*-
"""make_rs_phases.py — 生成 announce-rs 六阶段回放帧(本机运行, 需 cv2/numpy)

用法: python make_rs_phases.py <输出目录>
产物: phase_0.jpg .. phase_5.jpg
  0=空场 1=卡A 2=卡A 3=卡B 4=卡B 5=空场
  卡A=test_card.jpg (GA999->C01, GA200->C08)
  卡B=test_card_B.jpg (GA777->C03, GA999->C01)
帧内容与 selftest.py 各阶段一一对应, 推到靶机后由 selftest_rs.py 消费。
"""
import os
import sys

BASE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(BASE)
sys.path.insert(0, BASE)
from selftest import compose, imwrite_u, CARD_A, CARD_B  # noqa: E402


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    frames = [
        ("phase_0.jpg", compose(None)),
        ("phase_1.jpg", compose(CARD_A)),
        ("phase_2.jpg", compose(CARD_A)),
        ("phase_3.jpg", compose(CARD_B)),
        ("phase_4.jpg", compose(CARD_B)),
        ("phase_5.jpg", compose(None)),
    ]
    for name, img in frames:
        p = os.path.join(out, name)
        if not imwrite_u(p, img):
            print("写帧失败: " + p)
            return 1
        print("OK " + p)
    return 0


if __name__ == "__main__":
    sys.exit(main())
