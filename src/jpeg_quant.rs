//! Reads the quantization tables an MJPEG frame was encoded with.
//!
//! The artifact-reduction filter uses them to know, per DCT frequency, how
//! much information the encoder threw away, so it removes only what the
//! encoder could have introduced. Parsing walks the marker segments before the
//! scan data, a few hundred bytes per frame.

/// JPEG zigzag scan position -> natural (row-major, row = vertical frequency)
/// coefficient index.
const ZIGZAG_TO_NATURAL: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Libjpeg's standard luminance table (natural order) at quality 50, used
/// only to estimate the encoder's quality setting for display.
const STANDARD_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
    56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104,
    113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Quantization steps for the Y, Cb and Cr planes, natural coefficient order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JpegQuant {
    pub tables: [[u16; 64]; 3],
}

impl JpegQuant {
    /// Parses a baseline/extended 3-component JPEG. Returns `None` for
    /// anything else (grayscale, progressive, missing tables, truncated).
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return None;
        }
        let mut tables: [Option<[u16; 64]>; 4] = [None; 4];
        let mut selectors: Option<[usize; 3]> = None;
        let mut pos = 2;
        while pos + 4 <= data.len() {
            if data[pos] != 0xFF {
                return None;
            }
            let marker = data[pos + 1];
            if marker == 0xFF {
                pos += 1; // fill byte
                continue;
            }
            if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) {
                pos += 2;
                continue;
            }
            let length = usize::from(data[pos + 2]) << 8 | usize::from(data[pos + 3]);
            let segment = data.get(pos + 4..pos + 2 + length)?;
            match marker {
                0xDB => parse_dqt(segment, &mut tables)?,
                // Baseline and extended sequential DCT. Progressive and
                // lossless frames are not what capture cards send.
                0xC0 | 0xC1 => {
                    if segment.len() < 6 || segment[0] != 8 || segment[5] != 3 {
                        return None;
                    }
                    let component = |index: usize| segment.get(6 + 3 * index + 2).map(|t| usize::from(*t & 3));
                    selectors = Some([component(0)?, component(1)?, component(2)?]);
                }
                0xDA => break,
                _ => {}
            }
            pos += 2 + length;
        }
        let selectors = selectors?;
        Some(Self {
            tables: [tables[selectors[0]]?, tables[selectors[1]]?, tables[selectors[2]]?],
        })
    }

    /// Approximate libjpeg quality (1-100) of the luminance table, for display.
    pub fn estimated_quality(&self) -> u32 {
        let percent = self.tables[0]
            .iter()
            .zip(STANDARD_LUMA)
            .map(|(&q, standard)| f64::from(q) / f64::from(standard))
            .sum::<f64>()
            / 64.0
            * 100.0;
        let quality = if percent <= 100.0 { (200.0 - percent) / 2.0 } else { 5000.0 / percent };
        quality.round().clamp(1.0, 100.0) as u32
    }
}

fn parse_dqt(mut segment: &[u8], tables: &mut [Option<[u16; 64]>; 4]) -> Option<()> {
    while let Some((&header, rest)) = segment.split_first() {
        let sixteen_bit = header >> 4 != 0;
        let id = usize::from(header & 3);
        let size = if sixteen_bit { 128 } else { 64 };
        let values = rest.get(..size)?;
        let mut table = [0_u16; 64];
        for (zigzag, &natural) in ZIGZAG_TO_NATURAL.iter().enumerate() {
            table[natural] = if sixteen_bit {
                u16::from(values[2 * zigzag]) << 8 | u16::from(values[2 * zigzag + 1])
            } else {
                u16::from(values[zigzag])
            };
        }
        tables[id] = Some(table);
        segment = &rest[size..];
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 4:2:2 JPEG header: SOI, one DQT segment with `dqt` as its
    /// payload, SOF0 (Y uses table 0, Cb/Cr table 1), SOS.
    fn header_with_dqt(dqt: &[u8]) -> Vec<u8> {
        let length = (dqt.len() + 2) as u16;
        let mut data = vec![0xFF, 0xD8, 0xFF, 0xDB];
        data.extend_from_slice(&length.to_be_bytes());
        data.extend_from_slice(dqt);
        data.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 17, 8, 0x04, 0x38, 0x07, 0x80, 3, 1, 0x21, 0, 2, 0x11, 1, 3, 0x11, 1,
        ]);
        data.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        data
    }

    fn header(luma: [u8; 64], chroma: [u8; 64]) -> Vec<u8> {
        let mut dqt = vec![0x00];
        dqt.extend_from_slice(&luma);
        dqt.push(0x01);
        dqt.extend_from_slice(&chroma);
        header_with_dqt(&dqt)
    }

    #[test]
    fn reads_tables_in_natural_order_per_component() {
        let luma: [u8; 64] = std::array::from_fn(|i| i as u8 + 1);
        let chroma = [7_u8; 64];
        let quant = JpegQuant::parse(&header(luma, chroma)).unwrap();
        // Zigzag position 2 is natural index 8 (second row, first column).
        assert_eq!(quant.tables[0][0], 1);
        assert_eq!(quant.tables[0][1], 2);
        assert_eq!(quant.tables[0][8], 3);
        assert_eq!(quant.tables[0][63], 64);
        assert_eq!(quant.tables[1], [7; 64]);
        assert_eq!(quant.tables[2], [7; 64]);
    }

    #[test]
    fn estimates_libjpeg_quality() {
        let scaled = |quality: u32| -> [u8; 64] {
            let scale = if quality < 50 { 5000 / quality } else { 200 - 2 * quality };
            std::array::from_fn(|zigzag| {
                let natural = ZIGZAG_TO_NATURAL[zigzag];
                ((u32::from(STANDARD_LUMA[natural]) * scale + 50) / 100).clamp(1, 255) as u8
            })
        };
        for quality in [50, 75, 90] {
            let quant = JpegQuant::parse(&header(scaled(quality), [1; 64])).unwrap();
            assert!(quant.estimated_quality().abs_diff(quality) <= 2, "{quality}");
        }
    }

    #[test]
    fn reads_sixteen_bit_tables() {
        let mut dqt = vec![0x10]; // Pq = 1 (16-bit), table 0
        for step in 0..64_u16 {
            dqt.extend_from_slice(&(256 + step).to_be_bytes());
        }
        dqt.push(0x01);
        dqt.extend_from_slice(&[3; 64]);
        let quant = JpegQuant::parse(&header_with_dqt(&dqt)).unwrap();
        assert_eq!(quant.tables[0][0], 256);
        assert_eq!(quant.tables[0][8], 258); // zigzag position 2
        assert_eq!(quant.tables[0][63], 319);
        assert_eq!(quant.tables[1], [3; 64]);
    }

    #[test]
    fn rejects_truncated_and_non_jpeg_data() {
        let full = header([1; 64], [1; 64]);
        assert!(JpegQuant::parse(&full[..40]).is_none());
        assert!(JpegQuant::parse(b"not a jpeg").is_none());
    }
}
