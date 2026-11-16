use crate::Error;

// NOTE: This is the AST2100 video the BMC sends (RFB encoding 87): JPEG-like blocks and colour-coded ones. We decode as
// the BMC's own viewer does, bit for bit, except that we clamp samples and colours where it wraps them.

/// The screen as the frames so far painted it.
pub(crate) struct Decoder {
    pub(super) width: u16,
    pub(super) height: u16,
    image: Vec<u8>,
}

impl Decoder {
    /// An opaque black screen.
    pub(crate) fn new(width: u16, height: u16) -> Self {
        Self { width, height, image: [0, 0, 0, 255].repeat(usize::from(width) * usize::from(height)) }
    }

    /// Paints `payload` and says what it painted, as `[x, y, width, height]`, and why it stopped short, if it did.
    /// What a corrupt payload painted before it stopped stays.
    pub(crate) fn decode(&mut self, payload: &[u8]) -> (Vec<[u16; 4]>, Option<Error>) {
        let mut painted = Vec::new();
        let error = self.frame(payload, &mut painted).err();
        (painted, error)
    }

    /// Width × height × RGBA.
    pub(crate) fn image(&self) -> &[u8] {
        &self.image
    }

    fn frame(&mut self, payload: &[u8], painted: &mut Vec<[u16; 4]>) -> Result<(), Error> {
        let &[luma, chroma, m0, m1, ref stream @ ..] = payload else { return Err(corrupt("a header cut short")) };
        let is_subsampled = match u16::from_be_bytes([m0, m1]) {
            0x01A6 => true,
            0x01BC => false,
            mode => return Err(corrupt(format!("mode {mode:04X}"))),
        };

        // Normal and low quality, each for luma and chroma.
        let quantisations = [
            [LUMA.quantisation(luma)?, CHROMA.quantisation(chroma)?],
            [LUMA.quantisation(0)?, CHROMA.quantisation(0)?],
        ];

        // The units of an MCU by component: Y (four of them in 4:2:0), Cb, Cr.
        let (mcu, components) = if is_subsampled { (16, &[0, 0, 0, 0, 1, 2][..]) } else { (8, &[0, 1, 2][..]) };
        let (columns, rows) = (self.width.div_ceil(mcu), self.height.div_ceil(mcu));

        let mut bits = Bits { bytes: stream, ..Bits::default() };
        let (mut x, mut y) = (0, 0);
        let mut dc = [0; 3];
        let mut palette = [0x00_80_80, 0xFF_80_80, 0x80_80_80, 0xC0_80_80];
        let mut tile = [[0; 64]; 6];

        // We decode as many blocks as the viewer does before it gives up on a frame end.
        for _ in 0..=(usize::from(self.width) * usize::from(self.height) / 64).max(4096) {
            let code = bits.peek(4);
            match code {
                9 => return Ok(()),
                1..=3 | 0xA | 0xB => return Err(corrupt(format!("block code {code:X}"))),
                5..=7 | 0xD..=0xF if is_subsampled => return Err(corrupt("a colour-coded block in 4:2:0")),
                _ => {}
            }

            if code & 8 == 0 {
                bits.skip(4);
            } else {
                let position = bits.take(20);
                (x, y) = ((position >> 8 & 0xFF) as u16, (position & 0xFF) as u16);
            }

            if x >= columns || y >= rows {
                return Err(corrupt(format!("a block at ({x}, {y}), outside the screen")));
            }

            if matches!(code & 7, 0 | 4) {
                let quantisations = &quantisations[usize::from(code & 4 != 0)];
                for (unit, &component) in tile.iter_mut().zip(components) {
                    let kind = if component == 0 { &LUMA } else { &CHROMA };
                    let coefficients = kind.unit(&mut bits, &mut dc[component])?;
                    *unit = idct(&coefficients, &quantisations[component.min(1)]);
                }
            } else {
                // 1, 2 or 4 colours from the palette, each maybe replaced first, then 0, 1 or 2 bits a pixel.
                let depth = (code & 7) - 5;
                let mut chosen = [0; 4];
                for slot in &mut chosen[..1 << depth] {
                    let entry = bits.peek(27);
                    *slot = (entry >> 24 & 3) as usize;
                    if entry >> 26 == 0 {
                        bits.skip(3);
                    } else {
                        palette[*slot] = entry & 0xFF_FF_FF;
                        bits.skip(27);
                    }
                }

                let colours: [u32; 64] =
                    std::array::from_fn(|_| palette[chosen[if depth == 0 { 0 } else { bits.take(depth) as usize }]]);
                for (unit, shift) in tile.iter_mut().zip([16, 8, 0]) {
                    *unit = colours.map(|colour| (colour >> shift) as u8);
                }
            }

            if bits.read > 8 * stream.len() {
                return Err(corrupt("a frame cut short"));
            }

            let [left, top, width, height] = self.paint(x, y, mcu, &tile[..components.len()]);
            match painted.last_mut() {
                // We say what we painted per MCU row, from its first to its last painted block.
                Some(row) if row[1] == top => {
                    let right = (row[0] + row[2]).max(left + width);
                    row[0] = row[0].min(left);
                    row[2] = right - row[0];
                }
                _ => painted.push([left, top, width, height]),
            }

            x += 1;
            if x == columns {
                x = 0;
                y = (y + 1) % rows;
            }
        }

        Err(corrupt("a frame without an end"))
    }

    /// Paints the units of an MCU, Y (four of them in 4:2:0), Cb and Cr, at (x, y), clipped to the screen.
    fn paint(&mut self, x: u16, y: u16, mcu: u16, tile: &[[u8; 64]]) -> [u16; 4] {
        let (left, top) = (x * mcu, y * mcu);
        let (width, height) = (mcu.min(self.width - left), mcu.min(self.height - top));
        let (cb, cr) = (&tile[tile.len() - 2], &tile[tile.len() - 1]);
        let shift = usize::from(mcu / 16);

        for j in 0..usize::from(height) {
            let row = 4 * ((usize::from(top) + j) * usize::from(self.width) + usize::from(left));
            for i in 0..usize::from(width) {
                let c = (j >> shift) * 8 + (i >> shift);
                let rgb = rgb(tile[j / 8 * 2 + i / 8][j % 8 * 8 + i % 8], cb[c], cr[c]);
                self.image[row + 4 * i..][..3].copy_from_slice(&rgb);
            }
        }

        [left, top, width, height]
    }
}

fn corrupt(problem: impl std::fmt::Display) -> Error {
    Error::Protocol(format!("AST2100 video with {problem}"))
}

/// The bitstream: little-endian u32 words read from their most significant bit, zeros past the end.
#[derive(Default)]
struct Bits<'a> {
    bytes: &'a [u8],
    window: u64,
    filled: u32,
    next: usize,
    read: usize,
}

impl Bits<'_> {
    fn peek(&mut self, n: u32) -> u32 {
        while self.filled <= 32 {
            let mut word = [0; 4];
            let rest = self.bytes.get(self.next..).unwrap_or_default();
            let length = rest.len().min(4);
            word[..length].copy_from_slice(&rest[..length]);
            self.window |= u64::from(u32::from_le_bytes(word)) << (32 - self.filled);
            self.filled += 32;
            self.next += 4;
        }

        (self.window >> (64 - n)) as u32
    }

    fn skip(&mut self, n: u32) {
        self.peek(n);
        self.window <<= n;
        self.filled -= n;
        self.read += n as usize;
    }

    fn take(&mut self, n: u32) -> u32 {
        let bits = self.peek(n);
        self.skip(n);
        bits
    }

    /// JPEG's EXTEND: `n` bits as a signed value.
    fn signed(&mut self, n: u8) -> i32 {
        let bits = self.take(n.into()) as i32;
        if bits >> (n - 1) == 0 { bits - (1 << n) + 1 } else { bits }
    }
}

/// What luma and chroma each use: ITU T.81 Annex K's Huffman and quantisation tables, and the viewer's 12 qualities as
/// the factor n / 32 it scales the latter by.
struct Kind {
    dc: Huffman,
    ac: Huffman,
    quantisation: [u8; 64],
    steps: [u32; 12],
}

impl Kind {
    /// One 8 × 8 unit's coefficients in natural order: DC against the component's predictor, AC run-length coded.
    fn unit(&self, bits: &mut Bits, dc: &mut i32) -> Result<[i32; 64], Error> {
        let mut coefficients = [0; 64];

        let size = self.dc.decode(bits)?;
        if size > 0 {
            *dc += bits.signed(size);
        }
        coefficients[0] = *dc;

        let mut k = 1;
        while k < 64 {
            let symbol = self.ac.decode(bits)?;
            let (run, size) = (usize::from(symbol >> 4), symbol & 15);
            match size {
                0 if run < 15 => break,
                0 => k += 16,
                _ => {
                    k += run;
                    let &index = ZIGZAG.get(k).ok_or_else(|| corrupt("a coefficient past the 64th"))?;
                    coefficients[index] = bits.signed(size);
                    k += 1;
                }
            }
        }

        Ok(coefficients)
    }

    /// The quantisation table for a quality, prescaled for the AAN IDCT in f64 (left to right, as the viewer does).
    fn quantisation(&self, quality: u8) -> Result<[f64; 64], Error> {
        let &n = self.steps.get(usize::from(quality)).ok_or_else(|| corrupt(format!("quality {quality}")))?;
        let q = |i: usize| f64::from((u32::from(self.quantisation[i]) * n / 32).clamp(1, 255));

        Ok(std::array::from_fn(|i| q(i) * AAN[i / 8] * AAN[i % 8] * 65536.0))
    }
}

/// A canonical Huffman table as JPEG writes it down: how many codes there are of each length 1..=16, then their values.
struct Huffman {
    counts: [u8; 16],
    values: &'static [u8],
}

impl Huffman {
    fn decode(&self, bits: &mut Bits) -> Result<u8, Error> {
        let next = bits.peek(16);
        let (mut first, mut index) = (0, 0);
        for (length, count) in (1..).zip(self.counts.map(u32::from)) {
            let code = next >> (16 - length);

            // The table is canonical, so a code below `first` would have matched a shorter length.
            if code - first < count {
                bits.skip(length);
                return Ok(self.values[(index + code - first) as usize]);
            }

            index += count;
            first = (first + count) << 1;
        }

        Err(corrupt("an invalid Huffman code"))
    }
}

/// The viewer's integer AAN inverse DCT: libjpeg's jidctfst with 8 fractional bits, no descale between the passes, and
/// its 32-bit wraps where it wraps.
fn idct(coefficients: &[i32; 64], quantisation: &[f64; 64]) -> [u8; 64] {
    let mut work = [[0; 8]; 8];
    for c in 0..8 {
        let column = aan(std::array::from_fn(|r| {
            let i = r * 8 + c;
            i64::from(((f64::from(coefficients[i]) * quantisation[i]).trunc() as i64 as i32) >> 16)
        }));
        for (row, v) in work.iter_mut().zip(column) {
            row[c] = v;
        }
    }

    let mut samples = [0; 64];
    for (row, samples) in work.into_iter().zip(samples.as_chunks_mut().0) {
        *samples = aan(row).map(|v| ((v as i32 >> 3) + 128).clamp(0, 255) as u8);
    }

    samples
}

/// One pass over 8 values: an even and an odd half, then their butterflies.
fn aan(x: [i64; 8]) -> [i64; 8] {
    let mul = |v: i64, k: i64| i64::from((v * k) as i32 >> 8);

    let (t10, t11) = (x[0] + x[4], x[0] - x[4]);
    let t13 = x[2] + x[6];
    let t12 = mul(x[2] - x[6], 362) - t13;
    let (t0, t3, t1, t2) = (t10 + t13, t10 - t13, t11 + t12, t11 - t12);

    let (z13, z10, z11, z12) = (x[5] + x[3], x[5] - x[3], x[1] + x[7], x[1] - x[7]);
    let t7 = z11 + z13;
    let z5 = mul(z10 + z12, 473);
    let t6 = mul(z10, -669) + z5 - t7;
    let t5 = mul(z11 - z13, 362) - t6;
    let t4 = mul(z12, 277) - z5 + t5;

    [t0 + t7, t1 + t6, t2 + t5, t3 - t4, t3 + t4, t2 - t5, t1 - t6, t0 - t7]
}

/// BT.601 studio swing to RGB in the viewer's 16.16 fixed point (shifts floor). The viewer wraps sums above 447 to 0,
/// where we clamp.
fn rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let f = |k: i32, v: u8, offset: i32| (k * (i32::from(v) - offset) + 32768) >> 16;
    let y = f(76284, y, 16);

    [y + f(104704, cr, 128), y + f(-25601, cb, 128) + f(-53249, cr, 128), y + f(132097, cb, 128)]
        .map(|v| v.clamp(0, 255) as u8)
}

const AAN: [f64; 8] = [1.0, 1.387039845, 1.306562965, 1.175875602, 1.0, 0.785694958, 0.5411961, 0.275899379];

/// The natural index of each coefficient in zigzag order.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

const LUMA: Kind = Kind {
    dc: Huffman {
        counts: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
        values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    },
    ac: Huffman {
        counts: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D],
        values: &[
            0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
            0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72,
            0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
            0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83,
            0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3,
            0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
            0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
            0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
        ],
    },
    quantisation: [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29,
        51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121,
        120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ],
    steps: [40, 35, 29, 23, 18, 12, 6, 4, 4, 3, 2, 1],
};

const CHROMA: Kind = Kind {
    dc: Huffman {
        counts: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
        values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    },
    ac: Huffman {
        counts: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
        values: &[
            0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
            0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0, 0x15, 0x62, 0x72, 0xD1,
            0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x35, 0x36,
            0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
            0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A,
            0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A,
            0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA,
            0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
            0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
        ],
    },
    quantisation: [
        17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99,
    ],
    steps: [60, 52, 43, 35, 26, 18, 9, 6, 5, 4, 3, 2],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// `bits` (spaces ignored) packed as the viewer reads them.
    fn pack(bits: &str) -> Vec<u8> {
        let bits: Vec<u32> = bits.chars().filter(|c| !c.is_whitespace()).map(|c| u32::from(c == '1')).collect();
        bits.chunks(32)
            .flat_map(|word| word.iter().zip((0..32).rev()).fold(0, |w, (b, i)| w | b << i).to_le_bytes())
            .collect()
    }

    fn payload(header: [u8; 4], bits: &str) -> Vec<u8> {
        [header.to_vec(), pack(bits)].concat()
    }

    /// Seeded xorshift: a number below n each call.
    fn random(mut seed: u64) -> impl FnMut(u64) -> u64 {
        move |n| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        }
    }

    /// The pixel's RGB; its alpha has to be opaque.
    fn rgb_at(decoder: &Decoder, x: usize, y: usize) -> [u8; 3] {
        let offset = 4 * (y * usize::from(decoder.width) + x);
        assert_eq!(decoder.image[offset + 3], 255);
        decoder.image[offset..offset + 3].try_into().unwrap()
    }

    const FOUR_FOUR_FOUR: [u8; 4] = [7, 7, 0x01, 0xBC];

    #[test]
    fn huffman_codes_are_annex_ks() {
        let cases: [(&Huffman, &str, u8); 15] = [
            (&LUMA.dc, "00", 0),
            (&LUMA.dc, "010", 1),
            (&LUMA.dc, "110", 5),
            (&LUMA.dc, "1110", 6),
            (&LUMA.dc, "111111110", 11),
            (&LUMA.ac, "00", 0x01),
            (&LUMA.ac, "1010", 0x00),
            (&LUMA.ac, "11111111001", 0xF0),
            (&LUMA.ac, "1111111111111110", 0xFA),
            (&CHROMA.dc, "10", 2),
            (&CHROMA.dc, "11111111110", 11),
            (&CHROMA.ac, "00", 0x00),
            (&CHROMA.ac, "100", 0x02),
            (&CHROMA.ac, "1111111010", 0xF0),
            (&CHROMA.ac, "1111111111111110", 0xFA),
        ];
        for (table, code, value) in cases {
            let bytes = pack(code);
            let mut bits = Bits { bytes: &bytes, ..Bits::default() };
            assert_eq!(table.decode(&mut bits).unwrap(), value, "{code}");
            assert_eq!(bits.read, code.len(), "{code}");
        }
        for (table, code) in [(&LUMA.dc, "111111111"), (&LUMA.ac, "1111111111111111")] {
            let bytes = pack(code);
            assert!(table.decode(&mut Bits { bytes: &bytes, ..Bits::default() }).is_err(), "{code}");
        }
    }

    #[test]
    fn quant_tables_are_the_bmcs() {
        // The first and last rows of the viewer's tables for qualities 0, 4 (the X10's luma), 7 (its chroma) and 11.
        let cases: [(&Kind, u8, [u8; 16]); 8] = [
            (&LUMA, 0, [20, 13, 12, 20, 30, 50, 63, 76, 90, 115, 118, 122, 140, 125, 128, 123]),
            (&LUMA, 4, [9, 6, 5, 9, 13, 22, 28, 34, 40, 51, 53, 55, 63, 56, 57, 55]),
            (&LUMA, 7, [2, 1, 1, 2, 3, 5, 6, 7, 9, 11, 11, 12, 14, 12, 12, 12]),
            (&LUMA, 11, [1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3, 3]),
            (&CHROMA, 0, [31, 33, 45, 88, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185]),
            (&CHROMA, 4, [13, 14, 19, 38, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80]),
            (&CHROMA, 7, [3, 3, 4, 8, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18]),
            (&CHROMA, 11, [1, 1, 1, 2, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6]),
        ];
        for (kind, quality, rows) in cases {
            let table = kind.quantisation(quality).unwrap();
            for (i, q) in (0..8).chain(56..64).zip(rows) {
                assert_eq!(table[i], f64::from(q) * AAN[i / 8] * AAN[i % 8] * 65536.0, "quality {quality} [{i}]");
            }
        }
        assert!(LUMA.quantisation(12).is_err());
    }

    #[test]
    fn idct_is_near_the_exact_transform() {
        // The viewer truncates prescaled coefficients, which costs up to ~5 levels at the highest frequencies (the
        // golden frame pins those); DC and the 4 × 4 lowest stay within 2.
        let mut random = random(0x2545_F491_4F6C_DD1D);
        let quantisation = LUMA.quantisation(4).unwrap();
        let basis = |k: usize, position: usize| {
            let scale = if k == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
            scale * ((2 * position + 1) as f64 * k as f64 * std::f64::consts::PI / 16.0).cos()
        };
        for _ in 0..1000 {
            let mut coefficients = [0; 64];
            coefficients[0] = random(201) as i32 - 100;
            for _ in 0..6 {
                coefficients[(random(4) * 8 + random(4)) as usize] = random(21) as i32 - 10;
            }
            for (i, sample) in idct(&coefficients, &quantisation).into_iter().enumerate() {
                let exact: f64 = (0..64)
                    .map(|k| {
                        let q = quantisation[k] / (AAN[k / 8] * AAN[k % 8] * 65536.0);
                        f64::from(coefficients[k]) * q * basis(k / 8, i / 8) * basis(k % 8, i % 8) / 4.0
                    })
                    .sum();
                let exact = (exact + 128.0).clamp(0.0, 255.0);
                assert!((f64::from(sample) - exact).abs() < 2.0, "{coefficients:?} [{i}]: {sample} against {exact}");
            }
        }
    }

    #[test]
    fn colour_coded_blocks() {
        let mut decoder = Decoder::new(24, 16);
        let (painted, error) = decoder.decode(&payload(
            FOUR_FOUR_FOUR,
            &[
                // 1 colour at (0, 0): palette slot 1, white.
                "0101 0 01",
                // 2 colours at (1, 0): slot 0, black, and slot 2 replaced by Y 64; one bit a pixel, alternating.
                "0110 0 00 1 10 010000001000000010000000",
                &"01".repeat(32),
                // 4 colours at (2, 1): slots 0 to 3; two bits a pixel, cycling.
                "1111 00000010 00000001 0 00 0 01 0 10 0 11",
                &"00011011".repeat(16),
                "1001",
            ]
            .concat(),
        ));
        assert!(error.is_none(), "{error:?}");
        assert_eq!(painted, [[0, 0, 16, 8], [16, 8, 8, 8]]);
        for (x, y) in (0..64).map(|p| (p % 8, p / 8)) {
            assert_eq!(rgb_at(&decoder, x, y), [255; 3]);
            assert_eq!(rgb_at(&decoder, 8 + x, y), [[0; 3], [56; 3]][(y * 8 + x) % 2]);
            assert_eq!(rgb_at(&decoder, 16 + x, 8 + y), [[0; 3], [255; 3], [56; 3], [205; 3]][(y * 8 + x) % 4]);
            assert_eq!(rgb_at(&decoder, x, 8 + y), [0; 3], "untouched");
        }
    }

    #[test]
    fn low_quality_blocks_use_quality_0() {
        // DC 8 in luma, chroma 0, the second block predicting from the first: 8·q[0] / 8 + 128 is 148 at quality 0
        // (q 20), 130 at 7 (q 2).
        let mut decoder = Decoder::new(16, 8);
        let (painted, error) =
            decoder.decode(&payload(FOUR_FOUR_FOUR, "0100 101 1000 1010 00 00 00 00  0000 00 1010 00 00 00 00  1001"));
        assert!(error.is_none(), "{error:?}");
        assert_eq!(painted, [[0, 0, 16, 8]]);
        assert_eq!((rgb_at(&decoder, 7, 7), rgb_at(&decoder, 8, 0)), ([154; 3], [133; 3]));
    }

    #[test]
    fn corrupt_payloads_stop_after_what_they_painted() {
        let white = "0101 0 01";
        // A white block at (0, 0) first, then the trouble.
        let cases = [
            ("bad code", payload(FOUR_FOUR_FOUR, &[white, "1010"].concat()), 1),
            ("outside", payload(FOUR_FOUR_FOUR, &[white, "1101 00000011 00000000 0 01"].concat()), 1),
            ("cut short", payload(FOUR_FOUR_FOUR, &[white, "0000 101"].concat()), 1),
            // 4097 blocks, the most a frame holds, row after row of 3.
            ("no end", payload(FOUR_FOUR_FOUR, &white.repeat(5000)), 4097_usize.div_ceil(3)),
            ("colours in 4:2:0", payload([4, 7, 0x01, 0xA6], white), 0),
            ("quality 12", payload([12, 7, 0x01, 0xBC], white), 0),
            ("mode 0", payload([4, 7, 0, 0], white), 0),
            ("short", vec![4, 7, 0x01], 0),
        ];
        for (name, payload, rows) in cases {
            let mut decoder = Decoder::new(24, 16);
            let (painted, error) = decoder.decode(&payload);
            assert!(matches!(error, Some(Error::Protocol(_))), "{name}");
            assert_eq!(painted.len(), rows, "{name}");
            assert_eq!(rgb_at(&decoder, 0, 0), if rows > 0 { [255; 3] } else { [0; 3] }, "{name}");
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut random = random(0x9E37_79B9_7F4A_7C15);
        for _ in 0..3000 {
            let (width, height) = (random(40) as u16 + 1, random(40) as u16 + 1);
            let mut payload: Vec<u8> = (0..random(300)).map(|_| random(256) as u8).collect();
            if payload.len() >= 4 && random(5) > 0 {
                payload[..4].copy_from_slice(&[
                    random(12) as u8,
                    random(12) as u8,
                    1,
                    [0xA6, 0xBC][random(2) as usize],
                ]);
            }
            let mut decoder = Decoder::new(width, height);
            let (painted, _) = decoder.decode(&payload);
            assert!(painted.iter().all(|&[x, y, w, h]| x + w <= width && y + h <= height && w > 0 && h > 0));
        }
    }

    #[test]
    fn decodes_the_x10s_first_frame() {
        // The BMC's side opens with two 0x39 session messages of 265 bytes, then the first frame, 1024 × 768.
        let mock::Transcript(records) = mock::Transcript::load(&mock::fixture("x10").join("kvm.transcript")).unwrap();
        let bmc: Vec<u8> = records
            .into_iter()
            .filter(|(direction, _)| *direction == mock::Direction::Received)
            .flat_map(|(_, bytes)| bytes)
            .collect();
        let update = &bmc[2 * 265..];
        assert_eq!(update[..16], [0, 0, 0, 1, 0, 0, 0, 0, 4, 0, 3, 0, 0, 0, 0, 87]);
        let length = u32::from_be_bytes(update[20..24].try_into().unwrap()) as usize;
        let mut decoder = Decoder::new(1024, 768);
        let (painted, error) = decoder.decode(&update[24..][..length]);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(painted, (0..48).map(|row| [0, row * 16, 1024, 16]).collect::<Vec<_>>());
        // The BMC's own viewer, ast2100.js, paints the same.
        let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, decoder.image())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(hash, "93e8a7f3344b8737a3abd3f7756108ead6409648874ca174f3f088afaa8762fa");
    }
}
