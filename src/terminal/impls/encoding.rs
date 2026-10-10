use encoding_rs::{CoderResult, Encoding, UTF_8};

/// Stateful terminal decoder plus outbound encoder. Keeping the decoder alive
/// across SSH packets is important because a multibyte character may be split
/// between two channel data messages.
pub(crate) struct TerminalEncoding {
    encoding: &'static Encoding,
    decoder: encoding_rs::Decoder,
}

impl TerminalEncoding {
    pub(crate) fn new(label: &str) -> Self {
        let encoding = Encoding::for_label(label.trim().as_bytes()).unwrap_or(UTF_8);
        Self {
            encoding,
            decoder: encoding.new_decoder_without_bom_handling(),
        }
    }

    pub(crate) fn decode(&mut self, input: &[u8]) -> String {
        let mut output = String::with_capacity(input.len().saturating_mul(2).max(16));
        let mut offset = 0;
        loop {
            let (result, read, _) =
                self.decoder
                    .decode_to_string(&input[offset..], &mut output, false);
            offset += read;
            match result {
                CoderResult::InputEmpty => break,
                CoderResult::OutputFull => output.reserve(input.len().max(16)),
            }
        }
        output
    }

    pub(crate) fn encode(&self, input: &[u8]) -> Vec<u8> {
        if self.encoding == UTF_8 {
            return input.to_vec();
        }
        let text = String::from_utf8_lossy(input);
        let (encoded, _, _) = self.encoding.encode(&text);
        encoded.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gbk_round_trip_and_split_decode() {
        let encoder = TerminalEncoding::new("gbk");
        let bytes = encoder.encode("中文测试".as_bytes());
        let mut decoder = TerminalEncoding::new("gbk");
        let split = 3;
        let mut decoded = decoder.decode(&bytes[..split]);
        decoded.push_str(&decoder.decode(&bytes[split..]));
        assert_eq!(decoded, "中文测试");
    }

    #[test]
    fn every_session_dialog_label_is_a_known_encoding() {
        // Keep in sync with EncodingPicker in ui/session_dialog.slint (#435):
        // a typo there would silently decode as UTF-8.
        let dialog = include_str!("../../../ui/session_dialog.slint");
        let start = dialog
            .find("component EncodingPicker")
            .expect("EncodingPicker component");
        let model = &dialog[start..];
        let model = &model[model.find("model: [").unwrap() + 8..];
        let model = &model[..model.find(']').unwrap()];
        let labels: Vec<&str> = model
            .split(',')
            .map(|label| label.trim().trim_matches('"'))
            .filter(|label| !label.is_empty())
            .collect();
        assert!(labels.contains(&"GBK") && labels.contains(&"GB18030"));
        for label in labels {
            assert!(
                Encoding::for_label(label.as_bytes()).is_some(),
                "unknown encoding label in session dialog: {label}"
            );
        }
    }

    #[test]
    fn gbk_device_prompt_decodes_across_serial_sized_reads() {
        // A switch banner in GBK arriving one byte at a time (slow serial).
        let bytes = TerminalEncoding::new("GBK").encode("<交换机>显示 版本".as_bytes());
        let mut decoder = TerminalEncoding::new("GBK");
        let text: String = bytes.chunks(1).map(|b| decoder.decode(b)).collect();
        assert_eq!(text, "<交换机>显示 版本");
    }

    #[test]
    fn unknown_label_falls_back_to_utf8() {
        let mut codec = TerminalEncoding::new("not-a-real-charset");
        assert_eq!(codec.decode("你好".as_bytes()), "你好");
    }
}
