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
import os
import sys
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
        "tts": {"enabled": "on", "repeat": "1", "rate": "0"},
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
        一旦修复，本用例应改为断言"拒绝"。
        """
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
        """锁定"两处要同步改"的不变量：Python 表必须与 tts.rs 对齐。

        tts.rs: LETTER_CN = [('C',"西")]；DIGIT_CN 为 零..九。
        任一侧改了这里就红，提醒同步另一侧。
        """
        self.assertEqual(cw._CN_LETTER, {"C": "西"})
        self.assertEqual(
            cw._CN_DIGIT,
            {"0": "零", "1": "一", "2": "二", "3": "三", "4": "四",
             "5": "五", "6": "六", "7": "七", "8": "八", "9": "九"})


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

    def test_tts_repeat_floor(self):
        b = _base_body()
        b["tts"]["repeat"] = "0"
        cfg, _ = cw.validate_and_build(b)
        self.assertEqual(cfg["tts"]["repeat"], 1)

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


if __name__ == "__main__":
    unittest.main(verbosity=2)
