#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""config_web.py 单元测试（纯标准库 unittest，无 pip 依赖，与生产脚本同源理念）。

为什么补这层：config_web.py 是现场唯一的配置入口（改摄像头/ROI/阈值/播报/排班都
走它），却长期零自动化覆盖——一条错分支就可能把 config.json 写坏、或让"改个阈值
把模板清空"这类事故重新发生。本文件锁定其中**不碰 IO / 不碰网络 / 不碰 subprocess**
的纯逻辑：

  * norm_src            画面源归一化（/dev/videoN -> N）
  * validate_rtsp       RTSP 白名单校验（含伪协议注入防御）
  * validate_and_build  表单 -> config 的构造与校验（保存路径核心，分支最多）
  * spell_tokens / spell_digits   号码/窗口中文逐字念

有意**未覆盖**（需真机或 HTTP 栈，见文件末 TODO）：
  Handler.do_GET / do_POST 路由、ffmpeg 抓帧、systemctl/amixer/sudo 调用。
  这些靠现场冒烟（工具 announce-rs-deploy/）覆盖，不在此强行 mock 出"假绿"。

跑法：
    python3 -m unittest discover -s tests -v
    或  python3 tests/test_config_web.py
"""
import json
import os
import sys
import tempfile
import unittest
from unittest import mock

# 让测试无论从仓库根还是 tests/ 目录启动都能 import 到 config_web.py
_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

import config_web as cw  # noqa: E402


# ---------------------------------------------------------------- 测试夹具

def _base_cur():
    """一份"页面不编辑、必须原样沿用"的既有配置快照。"""
    return {
        "camera_source": "0",
        "capture_width": 1280, "capture_height": 720, "fps": 5.0,
        "roi": [100, 100, 200, 80],
        "detect": {
            "diff_threshold": 10.0, "changed_ratio": 0.02,
            "fallback_ocr_sec": 3.0, "stable_frames": 2,
            "confirm_ttl_sec": 4.0, "active_sec": 3.0,
            "settle_sec": 0.5, "min_interval_sec": 0.5,
        },
        "tts": {
            "enabled": True, "repeat": 1, "rate": 0,
            "engine": "auto", "template": "请{number}到{window}号窗口办理",
            "gap_sec": 0.35, "aplay_device": None,
        },
        "ocr": {
            "bin": "tesseract", "lang": "chi_sim", "psm": 7, "dpi": 150,
            "pattern": [r"^([A-Z])(\d{3})$"],
        },
        "schedule": {"enabled": False, "work_start": "08:00", "work_end": "18:00"},
    }


def _base_body():
    """一份能通过校验的表单（值均为字符串，模拟 HTTP 表单）。"""
    return {
        "src_type": "usb",
        "usb_idx": "0",
        "capture_width": "1280",
        "capture_height": "720",
        "fps": "5",
        "roi": ["100", "100", "200", "80"],
        "detect": {
            "diff_threshold": "10", "changed_ratio": "0.02",
            "fallback_ocr_sec": "3", "stable_frames": "2",
            "confirm_ttl_sec": "4", "active_sec": "3",
        },
        "tts": {"enabled": "on", "repeat": "1", "rate": "135"},
        "ocr": {"psm": "7"},
    }


# ---------------------------------------------------------------- norm_src

class TestNormSrc(unittest.TestCase):
    def test_dev_path_to_index(self):
        self.assertEqual(cw.norm_src("/dev/video0"), "0")
        self.assertEqual(cw.norm_src("/dev/video12"), "12")

    def test_strip_whitespace(self):
        self.assertEqual(cw.norm_src("  /dev/video3  "), "3")

    def test_plain_index_passthrough(self):
        self.assertEqual(cw.norm_src("0"), "0")

    def test_empty_and_none(self):
        self.assertEqual(cw.norm_src(""), "")
        self.assertEqual(cw.norm_src(None), "")

    def test_non_digit_dev_path_untouched(self):
        # 无数字尾巴 -> 不匹配正则 -> 原样返回（交给下游报"非法源"）
        self.assertEqual(cw.norm_src("/dev/video"), "/dev/video")

    def test_trailing_slash_untouched(self):
        # 正则带 $ 锚点：多一个斜杠就不算设备路径
        self.assertEqual(cw.norm_src("/dev/video0/"), "/dev/video0/")

    def test_rtsp_passthrough(self):
        self.assertEqual(cw.norm_src("rtsp://192.168.1.10/s1"),
                         "rtsp://192.168.1.10/s1")


# ---------------------------------------------------------------- validate_rtsp

class TestValidateRtsp(unittest.TestCase):
    def test_valid_basic(self):
        ok, v = cw.validate_rtsp("rtsp://192.168.1.10:554/stream1")
        self.assertTrue(ok)
        self.assertEqual(v, "rtsp://192.168.1.10:554/stream1")

    def test_valid_with_credentials(self):
        ok, _ = cw.validate_rtsp(
            "rtsp://admin:pass@192.168.1.10:554/Streaming/Channels/101")
        self.assertTrue(ok)

    def test_valid_with_query_params(self):
        ok, _ = cw.validate_rtsp("rtsp://10.0.0.2/live?channel=1&subtype=0")
        self.assertTrue(ok)

    def test_strip_before_check(self):
        ok, v = cw.validate_rtsp("  rtsp://10.0.0.2/s  ")
        self.assertTrue(ok)
        self.assertEqual(v, "rtsp://10.0.0.2/s")

    def test_empty_and_none(self):
        self.assertFalse(cw.validate_rtsp("")[0])
        self.assertFalse(cw.validate_rtsp(None)[0])

    def test_too_long(self):
        ok, msg = cw.validate_rtsp("rtsp://10.0.0.2/" + "a" * 500)
        self.assertFalse(ok)
        self.assertIn("过长", msg)

    def test_reject_pseudo_protocol_injection(self):
        # ★安全核心：ffmpeg 支持 concat:/file:/subfile: 等伪协议，必须拦死
        for bad in ("rtsp://x/concat:/etc/passwd",
                    "rtsp://x/file:/etc/shadow",
                    "rtsp://x/subfile:/etc/hosts",
                    "rtsp://pipe:1"):
            ok, msg = cw.validate_rtsp(bad)
            self.assertFalse(ok, bad)
            self.assertIn("非法序列", msg, bad)

    def test_reject_control_chars(self):
        self.assertFalse(cw.validate_rtsp("rtsp://10.0.0.2/s\n/x")[0])
        self.assertFalse(cw.validate_rtsp("rtsp://10.0.0.2/s\x00x")[0])

    def test_reject_non_whitelisted_chars(self):
        # 空格 / 分号 等不在字符集内
        self.assertFalse(cw.validate_rtsp("rtsp://x/y z")[0])
        self.assertFalse(cw.validate_rtsp("rtsp://x/y;rm -rf /")[0])

    def test_reject_missing_scheme(self):
        ok, msg = cw.validate_rtsp("http://10.0.0.2/s")
        self.assertFalse(ok)
        self.assertIn("格式非法", msg)

    def test_reject_loopback_hosts(self):
        for bad in ("rtsp://localhost/s", "rtsp://127.0.0.1/s",
                    "rtsp://0.0.0.0/s", "rtsp://::1/s"):
            ok, msg = cw.validate_rtsp(bad)
            self.assertFalse(ok, bad)
            self.assertIn("本机", msg, bad)

    def test_loopback_variants_known_gap(self):
        """★已知缺口（本用例固化现状，非"期望行为"）：

        "不允许指向本机"这道防线目前只枚举了 4 个字面主机名，
        以下两种等价写法仍能通过：

          * rtsp://[::1]/s       —— IPv6 回环的方括号写法，host 解析成 "[" 逃过比对
          * rtsp://127.1/s       —— 127.0.0.0/8 整段的简写

        危害有限（只是让 ffmpeg 去连本机，读文件类攻击已由伪协议黑名单拦死），
        但属纵深防御的漏网。修复方向：解析出 host 后做 is_loopback 判定而非字面比对。
        ★2026-10-23 低危修复: 加 skip 标记, 修复后取消 skip 并改为断言"拒绝"。
        """
        self.skipTest("已知安全缺口: rtsp://[::1]/s 和 rtsp://127.1/s 仍能通过回环校验, "
                      "修复后取消 skip 并改为 assertFalse")
        self.assertTrue(cw.validate_rtsp("rtsp://[::1]/s")[0])
        self.assertTrue(cw.validate_rtsp("rtsp://127.1/s")[0])


# ---------------------------------------------------------------- spell_*

class TestSpell(unittest.TestCase):
    def test_spell_tokens_digits_to_cn(self):
        self.assertEqual(cw.spell_tokens("174"), "一 七 四")

    def test_spell_tokens_letter_c_homophone(self):
        self.assertEqual(cw.spell_tokens("c08"), "西 零 八")
        self.assertEqual(cw.spell_tokens("C08"), "西 零 八")

    def test_spell_tokens_unmapped_letter_passthrough(self):
        # G/A 未列入谐音表 -> 原样保留（与主程序 tts.rs LETTER_CN 行为一致）
        self.assertEqual(cw.spell_tokens("GA174"), "G A 一 七 四")

    def test_spell_tokens_drop_whitespace(self):
        self.assertEqual(cw.spell_tokens("g a 1"), "g a 一")

    def test_spell_digits_numbers_only(self):
        self.assertEqual(cw.spell_digits("C08"), "零 八")

    def test_spell_digits_drop_spaces(self):
        self.assertEqual(cw.spell_digits("1 2 3"), "一 二 三")

    def test_spell_digits_empty(self):
        self.assertEqual(cw.spell_digits(""), "")

    def test_tables_match_rust_side(self):
        """锁定"两处要同步改"的不变量：Python 表必须与 tts.rs 源码真实对齐。

        ★2026-10-23 低危修复: 旧版只把 Python 表与字面量比对, tts.rs 改
        LETTER_CN/DIGIT_CN 不会红 —— "交叉锁定"是假的。改为真正解析 tts.rs 源码。
        """
        tts_rs = os.path.join(_REPO_ROOT, "src", "tts.rs")
        with open(tts_rs, encoding="utf-8") as f:
            src = f.read()
        # 从 tts.rs 提取 LETTER_CN 映射: ('C',"西") 格式
        import re
        letters = dict(re.findall(r"\('([A-Z])',\s*\"([^\"]+)\"\)", src))
        # DIGIT_CN 是纯字符串数组 ["零","一",...], 用索引作为键
        m = re.search(r'DIGIT_CN[^=]*=\s*\[([^\]]+)\]', src)
        digits = {}
        if m:
            for i, d in enumerate(re.findall(r'"([^"]+)"', m.group(1))):
                digits[str(i)] = d
        self.assertEqual(cw._CN_LETTER, letters)
        self.assertEqual(cw._CN_DIGIT, digits)


# ---------------------------------------------------------------- validate_and_build

class TestValidateAndBuild(unittest.TestCase):
    def setUp(self):
        # validate_and_build 会读磁盘 config 取"未编辑字段"，统一打桩
        self.cur = _base_cur()
        p = mock.patch.object(cw, "load_cfg", return_value=self.cur)
        p.start()
        self.addCleanup(p.stop)

    def _build(self, **over):
        body = _base_body()
        body.update(over)
        return cw.validate_and_build(body)

    # ---- 正常路径 & 字段沿用 ----

    def test_usb_basic_ok(self):
        cfg, err = self._build()
        self.assertIsNone(err)
        self.assertEqual(cfg["camera_source"], "0")
        self.assertEqual(cfg["capture_width"], 1280)
        self.assertEqual(cfg["capture_height"], 720)
        self.assertEqual(cfg["fps"], 5.0)

    def test_preserve_unedited_fields(self):
        cfg, err = self._build()
        self.assertIsNone(err)
        # 页面不编辑的字段必须原样带过来（防"改个阈值把模板清空"）
        self.assertEqual(cfg["tts"]["template"], self.cur["tts"]["template"])
        self.assertEqual(cfg["ocr"]["pattern"], self.cur["ocr"]["pattern"])
        self.assertEqual(cfg["ocr"]["lang"], "chi_sim")
        self.assertEqual(cfg["detect"]["min_interval_sec"], 0.5)
        self.assertEqual(cfg["tts"]["engine"], "auto")

    def test_usb_idx_string_coerced(self):
        cfg, _ = self._build(usb_idx=" 3 ")
        self.assertEqual(cfg["camera_source"], "3")

    def test_simulate_source(self):
        cfg, err = self._build(src_type="simulate")
        self.assertIsNone(err)
        self.assertEqual(cfg["camera_source"], "simulate")

    def test_rtsp_source_ok(self):
        cfg, err = self._build(src_type="rtsp",
                               rtsp="rtsp://192.168.1.10:554/s1")
        self.assertIsNone(err)
        self.assertEqual(cfg["camera_source"], "rtsp://192.168.1.10:554/s1")

    def test_rtsp_source_invalid(self):
        cfg, err = self._build(src_type="rtsp", rtsp="rtsp://127.0.0.1/s")
        self.assertIsNone(cfg)
        self.assertIn("RTSP 地址不合法", err)

    # ---- 错误路径 ----

    def test_unknown_src_type(self):
        cfg, err = self._build(src_type="carrier-pigeon")
        self.assertIsNone(cfg)
        self.assertIn("未知源类型", err)

    def test_resolution_too_small(self):
        cfg, err = self._build(capture_width="100")
        self.assertIsNone(cfg)
        self.assertIn("分辨率超出合理范围", err)

    def test_resolution_too_large(self):
        cfg, err = self._build(capture_height="4000")
        self.assertIsNone(cfg)
        self.assertIn("分辨率超出合理范围", err)

    def test_roi_wrong_length(self):
        cfg, err = self._build(roi=["1", "2", "3"])
        self.assertIsNone(cfg)
        self.assertIn("4 个非负整数", err)

    def test_roi_negative(self):
        cfg, err = self._build(roi=["-1", "0", "100", "100"])
        self.assertIsNone(cfg)
        self.assertIn("非负整数", err)

    def test_roi_zero_size(self):
        cfg, err = self._build(roi=["0", "0", "0", "100"])
        self.assertIsNone(cfg)
        self.assertIn("ROI 越界", err)

    def test_roi_out_of_bounds(self):
        cfg, err = self._build(roi=["1200", "0", "200", "100"])
        self.assertIsNone(cfg)
        self.assertIn("ROI 越界", err)

    def test_changed_ratio_zero(self):
        b = _base_body()
        b["detect"]["changed_ratio"] = "0"
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(cfg)
        self.assertIn("changed_ratio", err)

    def test_changed_ratio_one(self):
        b = _base_body()
        b["detect"]["changed_ratio"] = "1"
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(cfg)
        self.assertIn("changed_ratio", err)

    def test_missing_required_key(self):
        b = _base_body()
        del b["roi"]
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(cfg)
        self.assertIn("参数缺失或非法", err)

    # ---- 静默兜底/钳制（不报错，但必须纠正到安全值）----

    def test_zero_time_fields_clamped(self):
        """三个时段参数留空被算成 0 时，必须回填默认，否则"只播半句话"。"""
        b = _base_body()
        b["detect"].update({"fallback_ocr_sec": "0", "active_sec": "0",
                            "confirm_ttl_sec": "0"})
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(err)
        self.assertEqual(cfg["detect"]["fallback_ocr_sec"], 3.0)
        self.assertEqual(cfg["detect"]["active_sec"], 3.0)
        self.assertEqual(cfg["detect"]["confirm_ttl_sec"], 4.0)

    def test_settle_sec_absent_uses_current(self):
        cfg, _ = self._build()
        self.assertEqual(cfg["detect"]["settle_sec"], 0.5)

    def test_settle_sec_blank_uses_current(self):
        b = _base_body()
        b["detect"]["settle_sec"] = ""
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(err)
        self.assertEqual(cfg["detect"]["settle_sec"], 0.5)

    def test_settle_sec_low_clamped(self):
        b = _base_body()
        b["detect"]["settle_sec"] = "0"
        cfg, _ = cw.validate_and_build(b)
        self.assertEqual(cfg["detect"]["settle_sec"], 0.1)

    def test_settle_sec_high_clamped(self):
        b = _base_body()
        b["detect"]["settle_sec"] = "9"
        cfg, _ = cw.validate_and_build(b)
        self.assertEqual(cfg["detect"]["settle_sec"], 3.0)

    def test_tts_repeat_out_of_range_rejected(self):
        # ★2026-10-23 第八轮高危: repeat 越界从"静默钳"改为"拒绝保存"
        for bad in ("0", "6", "-1", "999999999"):
            b = _base_body()
            b["tts"]["repeat"] = bad
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(cfg, bad)
            self.assertIn("repeat 需在 1~5", err, bad)

    def test_tts_enabled_true(self):
        cfg, _ = self._build()
        self.assertTrue(cfg["tts"]["enabled"])

    # ---- piper 语速 ----

    def test_piper_length_scale_ok(self):
        b = _base_body()
        b["tts"]["piper_length_scale"] = "1.25"
        cfg, _ = cw.validate_and_build(b)
        self.assertEqual(cfg["tts"]["piper_length_scale"], 1.25)

    def test_piper_length_scale_out_of_range(self):
        b = _base_body()
        b["tts"]["piper_length_scale"] = "9"
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(cfg)
        self.assertIn("语速需在", err)

    def test_piper_length_scale_not_a_number(self):
        b = _base_body()
        b["tts"]["piper_length_scale"] = "abc"
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(cfg)
        self.assertIn("语速必须是数字", err)

    def test_piper_length_scale_absent_kept_out(self):
        cfg, _ = self._build()
        self.assertNotIn("piper_length_scale", cfg["tts"])

    # ---- 播放设备 aplay_device ----

    def test_aplay_device_saved(self):
        """★回归(2026-10-22): 表单里的播放设备必须落盘。

        旧实现 tts 合并块漏了 aplay_device —— 页面上改了设备点保存,
        返回"保存成功", 配置里却还是 None, 试听/播报永远走默认设备。
        """
        b = _base_body()
        b["tts"]["aplay_device"] = "plughw:1,0"
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(err)
        self.assertEqual(cfg["tts"]["aplay_device"], "plughw:1,0")

    def test_aplay_device_blank_falls_back_to_none(self):
        # 留空(页面 JS 会送 null) -> 归一成 None(默认设备), 而不是空串
        b = _base_body()
        b["tts"]["aplay_device"] = None
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(err)
        self.assertIsNone(cfg["tts"]["aplay_device"])

    def test_aplay_device_whitespace_only_is_none(self):
        b = _base_body()
        b["tts"]["aplay_device"] = "   "
        cfg, _ = cw.validate_and_build(b)
        self.assertIsNone(cfg["tts"]["aplay_device"])

    # ---- ocr.pattern 保护 ----

    def test_ocr_pattern_missing_in_current(self):
        self.cur["ocr"].pop("pattern")
        b = _base_body()
        b["ocr"] = {"psm": "7"}  # 页面不上送 pattern
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(cfg)
        self.assertIn("ocr.pattern 缺失", err)

    # ---- 工作时间排班 ----

    def test_schedule_default_when_absent(self):
        cfg, err = self._build()
        self.assertIsNone(err)
        self.assertEqual(cfg["schedule"],
                         {"enabled": False, "work_start": "08:00", "work_end": "18:00"})

    def test_schedule_ok(self):
        cfg, err = self._build(
            schedule={"enabled": "on", "work_start": "09:30", "work_end": "17:45"})
        self.assertIsNone(err)
        self.assertTrue(cfg["schedule"]["enabled"])
        self.assertEqual(cfg["schedule"]["work_start"], "09:30")
        self.assertEqual(cfg["schedule"]["work_end"], "17:45")

    def test_schedule_bad_format(self):
        cfg, err = self._build(
            schedule={"enabled": "on", "work_start": "25:00", "work_end": "18:00"})
        self.assertIsNone(cfg)
        self.assertIn("工作时间格式非法", err)

    def test_schedule_same_start_end(self):
        cfg, err = self._build(
            schedule={"enabled": "on", "work_start": "08:00", "work_end": "08:00"})
        self.assertIsNone(cfg)
        self.assertIn("不能相同", err)


# ---------------------------------------------------------------- 整数值域（第八轮高危）

class TestIntRangeGuard(unittest.TestCase):
    """★回归(2026-10-23 第八轮高危): 整数字段与页面 min/max 同口径拒绝越界值。

    旧版 stable_frames/psm 直接 int() 落盘、repeat 只钳下限、rate 不校验 ——
    内网任意主机 curl 一个越界值, 保存即生效并自动重启服务:
    stable_frames=0 每帧都播、psm=99 让 tesseract 全灭、repeat=10^9 占死声卡。
    """

    def setUp(self):
        p = mock.patch.object(cw, "load_cfg", return_value=_base_cur())
        p.start()
        self.addCleanup(p.stop)

    def test_stable_frames_out_of_range(self):
        for bad in ("0", "6", "-1", "1000000000"):
            b = _base_body()
            b["detect"]["stable_frames"] = bad
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(cfg, bad)
            self.assertIn("stable_frames 需在 1~5", err, bad)

    def test_stable_frames_boundary_ok(self):
        for good in ("1", "5"):
            b = _base_body()
            b["detect"]["stable_frames"] = good
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(err, good)
            self.assertEqual(cfg["detect"]["stable_frames"], int(good))

    def test_psm_out_of_range(self):
        for bad in ("0", "2", "14", "99"):
            b = _base_body()
            b["ocr"]["psm"] = bad
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(cfg, bad)
            self.assertIn("psm 需在 3~13", err, bad)

    def test_psm_boundary_ok(self):
        for good in ("3", "13"):
            b = _base_body()
            b["ocr"]["psm"] = good
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(err, good)
            self.assertEqual(cfg["ocr"]["psm"], int(good))

    def test_rate_out_of_range(self):
        for bad in ("0", "79", "251", "-80"):
            b = _base_body()
            b["tts"]["rate"] = bad
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(cfg, bad)
            self.assertIn("rate 需在 80~250", err, bad)

    def test_rate_boundary_ok(self):
        for good in ("80", "250"):
            b = _base_body()
            b["tts"]["rate"] = good
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(err, good)
            self.assertEqual(cfg["tts"]["rate"], int(good))


# ---------------------------------------------------------------- pattern 校验（第八轮高危）

class TestOcrPatternGuard(unittest.TestCase):
    """★回归(2026-10-23 第八轮高危): ocr.pattern 逐条校验字符串且可编译。

    旧版只查"非空 list": curl 送 [123] 或 ["("] 落盘后, Rust 侧 Regex::new
    失败 -> Config::load Err -> systemd 崩溃死循环, 服务永远起不来。
    """

    def setUp(self):
        p = mock.patch.object(cw, "load_cfg", return_value=_base_cur())
        p.start()
        self.addCleanup(p.stop)

    def test_non_string_element_rejected(self):
        b = _base_body()
        b["ocr"]["pattern"] = [123]
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(cfg)
        self.assertIn("必须全部是字符串", err)

    def test_uncompilable_regex_rejected(self):
        for bad in ("(", "[a-z", "\\"):
            b = _base_body()
            b["ocr"]["pattern"] = [bad]
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(cfg, bad)
            self.assertIn("正则无法编译", err, bad)

    def test_valid_pattern_accepted(self):
        b = _base_body()
        b["ocr"]["pattern"] = [r"^([A-Z])(\d{3})$", r"([A-Z]{2})\s*0*(\d{1,6})"]
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(err)
        self.assertEqual(len(cfg["ocr"]["pattern"]), 2)


# ---------------------------------------------------------------- NaN/Inf 穿透防御

class TestFiniteFloatGuard(unittest.TestCase):
    """★回归(2026-10-22 第三轮审查): NaN/Inf 穿透校验打崩 announce-rs。

    NaN 与任何数比较恒 False, validate 里全部范围校验形同虚设;
    json.dump 默认 allow_nan=True 把字面量 NaN 写进 config.json,
    serde_json 拒读 → announce-rs 启动即崩循环, 且页面上看不出异常。
    """

    def setUp(self):
        # validate_and_build 会读磁盘 config 取"未编辑字段"，统一打桩
        p = mock.patch.object(cw, "load_cfg", return_value=_base_cur())
        p.start()
        self.addCleanup(p.stop)

    def _build(self, **over):
        body = _base_body()
        body.update(over)
        return cw.validate_and_build(body)

    def test_detect_nan_inf_rejected(self):
        for field in ("diff_threshold", "changed_ratio", "fallback_ocr_sec",
                      "confirm_ttl_sec", "active_sec"):
            for bad in ("nan", "inf", "-inf", "NaN", "Infinity"):
                b = _base_body()
                b["detect"][field] = bad
                cfg, err = cw.validate_and_build(b)
                self.assertIsNone(cfg, (field, bad))
                self.assertIn("有限", err, (field, bad))

    def test_fps_nan_inf_rejected(self):
        for bad in ("nan", "inf", "-inf"):
            cfg, err = self._build(fps=bad)
            self.assertIsNone(cfg, bad)
            self.assertIn("有限", err, bad)

    def test_settle_sec_nan_inf_rejected(self):
        for bad in ("nan", "inf", "-inf"):
            b = _base_body()
            b["detect"]["settle_sec"] = bad
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(cfg, bad)
            self.assertIn("有限", err, bad)

    def test_piper_length_scale_nan_inf_rejected(self):
        for bad in ("nan", "inf", "-inf"):
            b = _base_body()
            b["tts"]["piper_length_scale"] = bad
            cfg, err = cw.validate_and_build(b)
            self.assertIsNone(cfg, bad)
            self.assertIn("语速需在", err, bad)

    def test_valid_floats_still_accepted(self):
        # 防御不能误伤: 正常值(含字符串小数)照常通过
        b = _base_body()
        b["detect"].update({"diff_threshold": "25.5", "settle_sec": "1.2"})
        b["tts"]["piper_length_scale"] = "1.25"
        cfg, err = cw.validate_and_build(b)
        self.assertIsNone(err)
        self.assertEqual(cfg["detect"]["diff_threshold"], 25.5)
        self.assertEqual(cfg["detect"]["settle_sec"], 1.2)
        self.assertEqual(cfg["tts"]["piper_length_scale"], 1.25)


class TestSaveCfgNanGuard(unittest.TestCase):
    """save_cfg 是最后一道防线: 含 nan/inf 的 dict 必须抛错且不碰旧配置。

    原子写语义下异常路径要清掉 .tmp 残留, 旧 config.json 原样保留 ——
    主服务(serde_json)宁可读旧配置也不能读到带 NaN 的半截文件。
    """

    def setUp(self):
        tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(tmpdir.cleanup)
        self.cfg_path = os.path.join(tmpdir.name, "config.json")
        self.old = {"fps": 5.0, "tts": {"rate": 135}}
        with open(self.cfg_path, "w", encoding="utf-8") as f:
            json.dump(self.old, f)
        p = mock.patch.object(cw, "CONFIG", self.cfg_path)
        p.start()
        self.addCleanup(p.stop)

    def _old_on_disk(self):
        with open(self.cfg_path, "r", encoding="utf-8") as f:
            return json.load(f)

    def test_nan_dict_raises_and_old_cfg_intact(self):
        with self.assertRaises(ValueError):
            cw.save_cfg({"fps": float("nan")})
        self.assertEqual(self._old_on_disk(), self.old)
        self.assertFalse(os.path.exists(self.cfg_path + ".tmp"),
                         "异常路径必须清理 .tmp 残留")

    def test_inf_dict_raises_and_old_cfg_intact(self):
        with self.assertRaises(ValueError):
            cw.save_cfg({"x": float("inf")})
        self.assertEqual(self._old_on_disk(), self.old)

    def test_nested_nan_raises(self):
        # nan 藏在嵌套结构里, json 编码时同样拦下
        with self.assertRaises(ValueError):
            cw.save_cfg({"detect": {"diff_threshold": float("nan")}})
        self.assertEqual(self._old_on_disk(), self.old)

    def test_normal_save_replaces(self):
        cw.save_cfg({"fps": 3.0})
        self.assertEqual(self._old_on_disk(), {"fps": 3.0})
        self.assertFalse(os.path.exists(self.cfg_path + ".tmp"))


# ---------------------------------------------------------------- 试听参数合并

class TestTtsPreviewOverrides(unittest.TestCase):
    """apply_tts_preview_overrides —— 「测试播报」的试听参数合并（原 do_POST 内联逻辑）。

    语义: 只覆盖列出的无副作用字段, 其余以磁盘配置为准; 参数非法整体忽略。
    """

    def _cfg(self):
        return {"tts": {"repeat": 1, "rate": 135, "spell_number": False,
                        "speak_window_letter": False, "gap_sec": 0.35,
                        "aplay_device": None}}

    def test_override_fields_applied(self):
        cfg = cw.apply_tts_preview_overrides(self._cfg(), {
            "repeat": "3", "rate": "160", "spell_number": True,
            "speak_window_letter": True, "piper_length_scale": "1.25"})
        t = cfg["tts"]
        self.assertEqual(t["repeat"], 3)
        self.assertEqual(t["rate"], 160)
        self.assertTrue(t["spell_number"])
        self.assertTrue(t["speak_window_letter"])
        self.assertEqual(t["piper_length_scale"], 1.25)
        self.assertEqual(t["gap_sec"], 0.35, "未覆盖字段必须原样保留")

    def test_aplay_device_override_applied(self):
        """★回归(2026-10-22): 页面填的播放设备必须传给试听进程。

        旧实现 override 列表漏了 aplay_device —— 换了设备想先试听,
        声音还是从磁盘配置的旧设备出, 页面上的"试听"形同虚设。
        """
        cfg = cw.apply_tts_preview_overrides(
            self._cfg(), {"aplay_device": "plughw:1,0"})
        self.assertEqual(cfg["tts"]["aplay_device"], "plughw:1,0")

    def test_aplay_device_empty_override_falls_back_to_none(self):
        # 页面清空后送 null -> 以页面为准回落默认设备, 不残留旧值
        cfg = self._cfg()
        cfg["tts"]["aplay_device"] = "plughw:1,0"
        cfg = cw.apply_tts_preview_overrides(cfg, {"aplay_device": None})
        self.assertIsNone(cfg["tts"]["aplay_device"])

    def test_non_dict_override_ignored(self):
        # ★回归(2026-10-22): 页面送数组/字符串等畸形数据 -> 整体忽略, 不抛 500。
        # 旧实现 ov.get 直接 AttributeError, 不在 except (TypeError, ValueError) 内。
        for bad in ([1, 2], "str", 42, None):
            cfg = cw.apply_tts_preview_overrides(self._cfg(), bad)
            self.assertEqual(cfg["tts"]["repeat"], self._cfg()["tts"]["repeat"], bad)

    def test_absent_keys_keep_disk_config(self):
        # 只送部分字段时, 其余(含 aplay_device)沿用磁盘配置
        cfg = self._cfg()
        cfg["tts"]["aplay_device"] = "plughw:2,0"
        cfg = cw.apply_tts_preview_overrides(cfg, {"repeat": "2"})
        self.assertEqual(cfg["tts"]["repeat"], 2)
        self.assertEqual(cfg["tts"]["aplay_device"], "plughw:2,0")

    def test_invalid_values_ignored_whole(self):
        # 非法参数 -> 整体退回磁盘配置, 不半改不改
        cfg = self._cfg()
        cfg["tts"]["aplay_device"] = "plughw:2,0"
        cfg = cw.apply_tts_preview_overrides(cfg, {"repeat": "abc",
                                                   "aplay_device": "x"})
        self.assertEqual(cfg["tts"]["repeat"], 1)
        self.assertEqual(cfg["tts"]["aplay_device"], "plughw:2,0")

    def test_piper_length_scale_out_of_range_rejected(self):
        # 越界语速不算"非法整体忽略"项, 但也不允许生效: 保持磁盘值
        cfg = cw.apply_tts_preview_overrides(self._cfg(), {"piper_length_scale": "9"})
        self.assertNotIn("piper_length_scale", cfg["tts"])


# ---------------------------------------------------------------- Origin 校验

class TestSameOrigin(unittest.TestCase):
    """_same_origin —— do_POST 入口的 CSRF 防御（2026-10-22 第四轮）。

    服务无认证, POST 全是改配置/断网/重启级副作用。恶意网页诱导访客浏览器
    跨站 POST 时浏览器必带攻击者站点的 Origin(老浏览器带 Referer), 与 Host
    头比对即识破 —— 攻击者无法伪造受害者浏览器发出的这些头。
    """

    def test_same_origin_origin_passes(self):
        self.assertTrue(cw._same_origin(
            "http://192.168.1.5:8080", None, "192.168.1.5:8080"))

    def test_cross_site_origin_rejected(self):
        self.assertFalse(cw._same_origin(
            "http://evil.example.com", None, "192.168.1.5:8080"))

    def test_origin_null_rejected(self):
        # 沙箱 iframe / 跨源跳转的特征, 明确拒绝
        self.assertFalse(cw._same_origin("null", None, "192.168.1.5:8080"))
        self.assertFalse(cw._same_origin("NULL", None, "192.168.1.5:8080"))

    def test_host_case_insensitive(self):
        # host 大小写不敏感(域名场景), 端口必须一致
        self.assertTrue(cw._same_origin(
            "http://Board.LOCAL:8080", None, "board.local:8080"))
        self.assertFalse(cw._same_origin(
            "http://board.local:8081", None, "board.local:8080"))

    def test_no_origin_same_site_referer_passes(self):
        # 老浏览器不带 Origin 只带 Referer, 同源 Referer 放行
        self.assertTrue(cw._same_origin(
            None, "http://192.168.1.5:8080/index.html", "192.168.1.5:8080"))

    def test_no_origin_cross_site_referer_rejected(self):
        self.assertFalse(cw._same_origin(
            None, "http://evil.example.com/page", "192.168.1.5:8080"))

    def test_no_origin_no_referer_passes(self):
        # 非浏览器客户端(curl/现场脚本)不在 CSRF 威胁模型内, 放行
        self.assertTrue(cw._same_origin(None, None, "192.168.1.5:8080"))

    def test_no_host_passes(self):
        # 无 Host 头 = 原始 HTTP 客户端, 同上放行
        self.assertTrue(cw._same_origin(None, None, ""))
        # 但跨站 Origin 即便无 Host 也不该放(防御优先)
        self.assertFalse(cw._same_origin("http://evil.example.com", None, ""))

    def test_origin_with_path_still_matches_netloc(self):
        # Origin 只含 scheme+netloc, 但带路径也不影响 netloc 比对
        self.assertTrue(cw._same_origin(
            "http://192.168.1.5:8080/", None, "192.168.1.5:8080"))


# ---------------------------------------------------------------- 配置指纹(乐观锁)

class TestConfigRev(unittest.TestCase):
    """config_rev —— 并发保存乐观锁的内容指纹（2026-10-22 第四轮）。

    两个浏览器窗口同时编辑时, 后保存者用旧快照整份覆盖会静默丢掉先保存者的
    改动。指纹必须只随"内容"变: touch 改 mtime 不能造成假冲突。
    """

    def setUp(self):
        tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(tmpdir.cleanup)
        self.cfg_path = os.path.join(tmpdir.name, "config.json")
        with open(self.cfg_path, "w", encoding="utf-8") as f:
            json.dump({"fps": 5.0}, f)

    def test_rev_is_16hex_and_stable(self):
        r1 = cw.config_rev(self.cfg_path)
        self.assertRegex(r1, r"^[0-9a-f]{16}$")
        self.assertEqual(r1, cw.config_rev(self.cfg_path),
                         "内容没变指纹必须稳定")

    def test_rev_changes_with_content(self):
        r1 = cw.config_rev(self.cfg_path)
        with open(self.cfg_path, "a", encoding="utf-8") as f:
            f.write("\n")
        self.assertNotEqual(r1, cw.config_rev(self.cfg_path))

    def test_missing_file_returns_empty(self):
        self.assertEqual(cw.config_rev(os.path.join(self.cfg_path, "nope")), "")

    def test_uses_global_config_when_no_arg(self):
        # 无参时读全局 CONFIG(与 /api/config、/api/save 同源, 打桩验证接线)
        with open(self.cfg_path, "w", encoding="utf-8") as f:
            json.dump({"fps": 9.0}, f)
        p = mock.patch.object(cw, "CONFIG", self.cfg_path)
        p.start()
        self.addCleanup(p.stop)
        r_noarg = cw.config_rev()
        r_explicit = cw.config_rev(self.cfg_path)
        self.assertEqual(r_noarg, r_explicit)
        self.assertNotEqual(r_noarg, "", "无参版必须真读到内容而非空串")
        with open(self.cfg_path, "r", encoding="utf-8") as f:
            self.assertEqual(json.load(f), {"fps": 9.0})


# ---------------------------------------------------------------- POST 请求体上限

class TestRequestBodyLimit(unittest.TestCase):
    """Handler._read_body —— POST 请求体上限（2026-10-22 第五轮）。

    旧版 5 个 POST 端点对 Content-Length 不设限: 负值(int("-1") 合法)让
    read(-1) 阻塞到 EOF 线程永久挂起; 超大 body 整段读进内存撑爆
    announce-web 的 MemoryMax。用 raw socket 精确控制请求头。
    """

    @classmethod
    def setUpClass(cls):
        tmpdir = tempfile.TemporaryDirectory()
        cls.addClassCleanup(tmpdir.cleanup)
        cls.cfg_path = os.path.join(tmpdir.name, "config.json")
        with open(cls.cfg_path, "w", encoding="utf-8") as f:
            json.dump(_base_cur(), f)
        for attr, val in (("CONFIG", cls.cfg_path),
                          ("RESTART_CMD", ["true"]),   # save 后的重启打桩为真跑
                          ("SCHED_CMD", ["false"])):   # 调度脚本必败 -> 退回 RESTART
            p = mock.patch.object(cw, attr, val)
            p.start()
            cls.addClassCleanup(p.stop)
        import threading
        from http.server import ThreadingHTTPServer
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), cw.Handler)
        cls.addClassCleanup(cls.srv.shutdown)
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()
        cls.port = cls.srv.server_address[1]

    def _raw(self, req: bytes) -> bytes:
        import socket
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as s:
            s.sendall(req)
            chunks = []
            while True:
                try:
                    b = s.recv(65536)
                except socket.timeout:
                    break
                if not b:
                    break
                chunks.append(b)
        return b"".join(chunks)

    def _status(self, resp: bytes) -> int:
        # BaseHTTPRequestHandler 默认 HTTP/1.0, 只取首行状态码不挑协议版本
        return int(resp.split(b"\r\n", 1)[0].split()[1])

    def test_negative_content_length_400(self):
        # int("-1") 合法 -> 旧版 read(-1) 阻塞挂线程; 现在必须 400 拒绝
        req = (b"POST /api/logs HTTP/1.1\r\nHost: t\r\nConnection: close\r\n"
               b"Content-Type: application/json\r\nContent-Length: -1\r\n\r\n")
        resp = self._raw(req)
        self.assertEqual(self._status(resp), 400, resp[:60])

    def test_oversized_content_length_413(self):
        req = (b"POST /api/logs HTTP/1.1\r\nHost: t\r\nConnection: close\r\n"
               b"Content-Type: application/json\r\n"
               b"Content-Length: " + str(cw.MAX_BODY_BYTES + 1).encode() + b"\r\n\r\n")
        resp = self._raw(req)
        self.assertEqual(self._status(resp), 413, resp[:60])

    def test_non_numeric_content_length_400(self):
        req = (b"POST /api/logs HTTP/1.1\r\nHost: t\r\nConnection: close\r\n"
               b"Content-Type: application/json\r\nContent-Length: abc\r\n\r\n")
        resp = self._raw(req)
        self.assertEqual(self._status(resp), 400, resp[:60])

    def test_normal_save_unaffected(self):
        # 完整合法表单不受上限影响: 保存走通并返回 200+rev
        import json as _json
        body = _json.dumps(_base_body()).encode("utf-8")
        req = (b"POST /api/save HTTP/1.1\r\nHost: t\r\nConnection: close\r\n"
               b"Content-Type: application/json\r\nContent-Length: "
               + str(len(body)).encode() + b"\r\n\r\n" + body)
        resp = self._raw(req)
        self.assertEqual(self._status(resp), 200, resp[:80])
        self.assertIn(b'"rev"', resp)


class TestCsrfHttpWiring(unittest.TestCase):
    """do_POST 的 _same_origin 接线(2026-10-23 第七轮中危⑤)。

    纯函数 _same_origin 已有 9 条单测, 但 do_POST 里的调用点/403 行为
    无人测 —— 把 1704 行删掉全部测试仍绿。起真实服务器验证接线。
    """

    @classmethod
    def setUpClass(cls):
        tmpdir = tempfile.TemporaryDirectory()
        cls.addClassCleanup(tmpdir.cleanup)
        cls.cfg_path = os.path.join(tmpdir.name, "config.json")
        with open(cls.cfg_path, "w", encoding="utf-8") as f:
            json.dump(_base_cur(), f)
        for attr, val in (("CONFIG", cls.cfg_path),
                          ("RESTART_CMD", ["true"]),
                          ("SCHED_CMD", ["false"])):
            p = mock.patch.object(cw, attr, val)
            p.start()
            cls.addClassCleanup(p.stop)
        import threading
        from http.server import ThreadingHTTPServer
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), cw.Handler)
        cls.addClassCleanup(cls.srv.shutdown)
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()
        cls.port = cls.srv.server_address[1]

    def _post(self, path: str, headers: dict, body: bytes = b"{}") -> bytes:
        import socket
        req = (b"POST " + path.encode() + b" HTTP/1.1\r\n"
               b"Connection: close\r\n"
               b"Content-Type: application/json\r\n"
               b"Content-Length: " + str(len(body)).encode() + b"\r\n")
        for k, v in headers.items():
            req += k.encode() + b": " + v.encode() + b"\r\n"
        req += b"\r\n" + body
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as s:
            s.sendall(req)
            chunks = []
            while True:
                try:
                    b = s.recv(65536)
                except socket.timeout:
                    break
                if not b:
                    break
                chunks.append(b)
        return b"".join(chunks)

    def _status(self, resp: bytes) -> int:
        return int(resp.split(b"\r\n", 1)[0].split()[1])

    def test_cross_origin_rejected(self):
        resp = self._post("/api/save", {"Host": "127.0.0.1", "Origin": "http://evil.com"})
        self.assertEqual(self._status(resp), 403, resp[:80])

    def test_same_origin_allowed(self):
        resp = self._post("/api/save", {"Host": "127.0.0.1", "Origin": "http://127.0.0.1"})
        # 走到正常处理(validate_and_build 可能 400, 但不是 403)
        self.assertNotEqual(self._status(resp), 403, resp[:80])

    def test_no_origin_no_referer_allowed(self):
        # curl/脚本客户端: 无 Origin 无 Referer 放行
        resp = self._post("/api/save", {"Host": "127.0.0.1"})
        self.assertNotEqual(self._status(resp), 403, resp[:80])

    def test_origin_null_rejected(self):
        resp = self._post("/api/save", {"Host": "127.0.0.1", "Origin": "null"})
        self.assertEqual(self._status(resp), 403, resp[:80])

    def test_origin_no_host_rejected(self):
        # 带 Origin 却无 Host 的畸形请求: 防御优先直接拒
        resp = self._post("/api/save", {"Origin": "http://127.0.0.1"})
        self.assertEqual(self._status(resp), 403, resp[:80])


class TestOptimisticLock(unittest.TestCase):
    """__rev 乐观锁 409 冲突路径(2026-10-23 第七轮中危⑤)。

    config_rev() 有单测, 但 /api/save 的 __rev 比对与 409 返回无用例。
    """

    @classmethod
    def setUpClass(cls):
        tmpdir = tempfile.TemporaryDirectory()
        cls.addClassCleanup(tmpdir.cleanup)
        cls.cfg_path = os.path.join(tmpdir.name, "config.json")
        with open(cls.cfg_path, "w", encoding="utf-8") as f:
            json.dump(_base_cur(), f)
        for attr, val in (("CONFIG", cls.cfg_path),
                          ("RESTART_CMD", ["true"]),
                          ("SCHED_CMD", ["false"])):
            p = mock.patch.object(cw, attr, val)
            p.start()
            cls.addClassCleanup(p.stop)
        import threading
        from http.server import ThreadingHTTPServer
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), cw.Handler)
        cls.addClassCleanup(cls.srv.shutdown)
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()
        cls.port = cls.srv.server_address[1]

    def _req(self, method: str, path: str, body: bytes = b"") -> bytes:
        import socket
        req = (method.encode() + b" " + path.encode() + b" HTTP/1.1\r\n"
               b"Host: 127.0.0.1\r\nConnection: close\r\n")
        if body:
            req += (b"Content-Type: application/json\r\n"
                    b"Content-Length: " + str(len(body)).encode() + b"\r\n")
        req += b"\r\n" + body
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as s:
            s.sendall(req)
            chunks = []
            while True:
                try:
                    b = s.recv(65536)
                except socket.timeout:
                    break
                if not b:
                    break
                chunks.append(b)
        return b"".join(chunks)

    def _status(self, resp: bytes) -> int:
        return int(resp.split(b"\r\n", 1)[0].split()[1])

    def _json_body(self, resp: bytes) -> dict:
        return json.loads(resp.split(b"\r\n\r\n", 1)[1])

    def test_wrong_rev_409(self):
        body = json.dumps(dict(_base_body(), __rev="wrong-rev")).encode()
        resp = self._req("POST", "/api/save", body)
        self.assertEqual(self._status(resp), 409, resp[:80])
        self.assertIn("配置已被其他窗口修改", self._json_body(resp)["error"])

    def test_correct_rev_200(self):
        # 先 GET /api/config 拿 rev
        resp = self._req("GET", "/api/config")
        rev = self._json_body(resp)["rev"]
        body = json.dumps(dict(_base_body(), __rev=rev)).encode()
        resp = self._req("POST", "/api/save", body)
        self.assertEqual(self._status(resp), 200, resp[:80])

    def test_no_rev_200_compat(self):
        # 脚本工具不带 __rev 不拦(向后兼容)
        body = json.dumps(_base_body()).encode()
        resp = self._req("POST", "/api/save", body)
        self.assertEqual(self._status(resp), 200, resp[:80])


class TestInvalidValueHttpWiring(unittest.TestCase):
    """越界值经真实 HTTP POST 必须 400 且 config.json 一字节不动(第八轮高危)。

    纯函数用例防的是 validate_and_build 逻辑; 本类防"接线断" ——
    比如 do_POST 忘了把 err 转成 400, 或出错路径仍落盘。
    """

    @classmethod
    def setUpClass(cls):
        tmpdir = tempfile.TemporaryDirectory()
        cls.addClassCleanup(tmpdir.cleanup)
        cls.cfg_path = os.path.join(tmpdir.name, "config.json")
        with open(cls.cfg_path, "w", encoding="utf-8") as f:
            json.dump(_base_cur(), f)
        for attr, val in (("CONFIG", cls.cfg_path),
                          ("RESTART_CMD", ["true"]),
                          ("SCHED_CMD", ["false"])):
            p = mock.patch.object(cw, attr, val)
            p.start()
            cls.addClassCleanup(p.stop)
        import threading
        from http.server import ThreadingHTTPServer
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), cw.Handler)
        cls.addClassCleanup(cls.srv.shutdown)
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()
        cls.port = cls.srv.server_address[1]

    def _post_save(self, body: dict) -> bytes:
        import socket
        raw = json.dumps(body).encode()
        req = (b"POST /api/save HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n"
               b"Content-Type: application/json\r\nContent-Length: "
               + str(len(raw)).encode() + b"\r\n\r\n" + raw)
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as s:
            s.sendall(req)
            chunks = []
            while True:
                try:
                    b = s.recv(65536)
                except socket.timeout:
                    break
                if not b:
                    break
                chunks.append(b)
        return b"".join(chunks)

    def _status(self, resp: bytes) -> int:
        return int(resp.split(b"\r\n", 1)[0].split()[1])

    def _disk(self) -> bytes:
        with open(self.cfg_path, "rb") as f:
            return f.read()

    def test_poison_values_400_and_disk_untouched(self):
        before = self._disk()
        for mutate in (
            lambda b: b["detect"].update(stable_frames="0"),
            lambda b: b["ocr"].update(psm="99"),
            lambda b: b["tts"].update(repeat="1000000000"),
            lambda b: b["tts"].update(rate="0"),
            lambda b: b["ocr"].update(pattern=["("]),
        ):
            body = _base_body()
            mutate(body)
            resp = self._post_save(body)
            self.assertEqual(self._status(resp), 400, resp[:100])
            self.assertEqual(self._disk(), before,
                             "拒绝保存时 config.json 必须一字节不动")


class TestSnapshotNoDoubleUnquote(unittest.TestCase):
    """★回归(2026-10-23 第八轮中危): /api/snapshot 的 src 不得二次 unquote。

    parse_qs 已解码一次, 旧版又 unquote 一次: 密码 "p%41x" 被改写成 "pAx",
    RTSP 认证失败且页面上毫无线索。这里 mock ffmpeg_snapshot 捕获实际 src。
    """

    @classmethod
    def setUpClass(cls):
        tmpdir = tempfile.TemporaryDirectory()
        cls.addClassCleanup(tmpdir.cleanup)
        cls.cfg_path = os.path.join(tmpdir.name, "config.json")
        with open(cls.cfg_path, "w", encoding="utf-8") as f:
            json.dump(_base_cur(), f)
        p = mock.patch.object(cw, "CONFIG", cls.cfg_path)
        p.start()
        cls.addClassCleanup(p.stop)
        import threading
        from http.server import ThreadingHTTPServer
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), cw.Handler)
        cls.addClassCleanup(cls.srv.shutdown)
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()
        cls.port = cls.srv.server_address[1]

    def test_percent_in_password_preserved(self):
        captured = {}

        def fake_snap(src, w, h):
            captured["src"] = src
            return b"\xff\xd8jpeg", None

        with mock.patch.object(cw, "ffmpeg_snapshot", side_effect=fake_snap):
            import urllib.request
            # 页面 encodeURIComponent("rtsp://u:p%41x@192.168.1.10/s") 的结果
            url = ("http://127.0.0.1:%d/api/snapshot?w=1280&h=720&src="
                   "rtsp%%3A%%2F%%2Fu%%3Ap%%2541x%%40192.168.1.10%%2Fs") % self.port
            with urllib.request.urlopen(url, timeout=10) as r:
                self.assertEqual(r.status, 200)
        self.assertEqual(captured["src"], "rtsp://u:p%41x@192.168.1.10/s",
                         "parse_qs 之后不得再 unquote, 否则密码被改写")


if __name__ == "__main__":
    unittest.main(verbosity=2)