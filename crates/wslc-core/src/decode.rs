//! 输出解码：`wslc` 默认吐 UTF-16LE，注入 `WSL_UTF8=1` 后才是 UTF-8。
//!
//! 实机验证（`docs/wslc-schema.md` §0.1）：
//!
//! ```text
//! cmd /c "wslc info"                   → UTF-16LE
//! cmd /c "set WSL_UTF8=1&& wslc info"  → UTF-8（E5 AE A2 = "客"）
//! ```
//!
//! 正常情况下 [`crate::cli::Wslc`] 会注入 `WSL_UTF8=1`，
//! 但**不能假设它一定生效**（用户可能自己包装了 wslc、或未来版本改行为），
//! 所以这里仍然做完整的编码判定，避免界面出现乱码或 `\0` 字符。

/// 把子进程输出的原始字节解码成 `String`。
///
/// 判定顺序：
///
/// 1. 有 BOM → 按 BOM 指定的编码解码（UTF-8 / UTF-16LE / UTF-16BE）。
/// 2. 能按 UTF-8 严格解码**且结果不含 NUL** → 判定为 UTF-8。
/// 3. 长度为偶数 → 判定为无 BOM 的 UTF-16LE。
/// 4. 其余情况 → `from_utf8_lossy` 兜底。
///
/// 第 2 步是关键：UTF-16LE 的 ASCII 文本（`W\0S\0L\0`）在 UTF-8 下**也是合法**的，
/// 但一定含 NUL；而 UTF-16LE 的中文（`客` = `A2 5B`）在 UTF-8 下是非法序列。
/// 两条判据合起来可以覆盖 `wslc` 的实际输出形态。
///
/// 任何路径都不会 panic；无法解码的字节替换为 U+FFFD。
pub fn decode(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    // 1) 显式 BOM 优先。
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&bytes[2..], true);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&bytes[2..], false);
    }
    let body = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };

    // 2) 严格 UTF-8 且无 NUL → 就是 UTF-8。
    if let Ok(text) = std::str::from_utf8(body) {
        if !text.as_bytes().contains(&0) {
            return text.to_owned();
        }
    }

    // 3) 偶数长度 → 无 BOM 的 UTF-16LE。
    if body.len() % 2 == 0 {
        return decode_utf16(body, true);
    }

    // 4) 兜底。
    String::from_utf8_lossy(body).into_owned()
}

/// 解码 UTF-16 字节序列，丢弃末尾不完整的半个码元。
fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// 把一段文本编码成 UTF-16LE 字节（**仅供测试使用**，用于构造 fixture）。
#[doc(hidden)]
pub fn encode_utf16le(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// 把一段文本编码成 UTF-16LE 字节并加上 BOM（**仅供测试使用**）。
#[doc(hidden)]
pub fn encode_utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    out.extend_from_slice(&encode_utf16le(text));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_plain_utf8() {
        assert_eq!(decode("客户端".as_bytes()), "客户端");
        assert_eq!(decode(b"{\"ID\":\"abc\"}"), "{\"ID\":\"abc\"}");
    }

    #[test]
    fn decodes_utf8_with_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("版本".as_bytes());
        assert_eq!(decode(&bytes), "版本");
    }

    #[test]
    fn decodes_utf16le_with_bom() {
        let bytes = encode_utf16le_with_bom("客户端: WSL 3.0.1.0\r\n");
        assert_eq!(decode(&bytes), "客户端: WSL 3.0.1.0\r\n");
    }

    #[test]
    fn decodes_utf16le_without_bom_mixed_ascii_and_cjk() {
        // 这正是未设置 WSL_UTF8 时 `wslc info` 的实际输出形态。
        let bytes = encode_utf16le("客户端:\r\nWSL 版本: 3.0.1.0\r\n");
        assert_eq!(decode(&bytes), "客户端:\r\nWSL 版本: 3.0.1.0\r\n");
    }

    #[test]
    fn decodes_utf16le_without_bom_pure_cjk() {
        // 纯中文的 UTF-16LE：高位字节非 0，靠“UTF-8 严格解码失败”判定。
        let bytes = encode_utf16le("容器映像网络卷");
        assert_eq!(decode(&bytes), "容器映像网络卷");
    }

    #[test]
    fn decodes_utf16le_without_bom_and_without_bom_pure_ascii() {
        // 纯 ASCII 的 UTF-16LE：含 NUL，靠“无 NUL”判据排除 UTF-8。
        let bytes = encode_utf16le("Up 3 seconds");
        assert_eq!(decode(&bytes), "Up 3 seconds");
    }

    #[test]
    fn does_not_misdetect_real_jsonl_output_as_utf16() {
        // 真实采集到的容器的 JSON 行（偶数长度），必须是 UTF-8。
        let line = concat!(
            r#"{"Command":"\"sleep 300\"","CreatedAt":"2026-10-08 16:34:46 +0800 GMT+8","#,
            r#""ID":"ff0667ee90fb","Names":"wslc-panel-probe","State":"running"}"#
        );
        assert!(
            line.len() % 2 == 0,
            "测试样本应恰好是偶数长度以覆盖误判场景"
        );
        assert_eq!(decode(line.as_bytes()), line);
    }

    #[test]
    fn handles_empty() {
        assert_eq!(decode(b""), "");
    }

    #[test]
    fn odd_length_falls_back_to_lossy_utf8() {
        assert_eq!(decode(&[0x41, 0x00, 0x42]), "A\0B");
    }

    #[test]
    fn lossy_on_invalid_utf8_odd_length() {
        // 奇数长度、非法 UTF-8 → 不 panic，替换成 U+FFFD。
        assert!(decode(&[0xA2, 0x5B, 0xFF]).contains('\u{FFFD}'));
    }
}
