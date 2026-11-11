use crate::device;
use crate::ui;

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
pub struct DecodedCheat {
    code_type: u8,
    address: u32,
    data: u16,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Cheats {
    pub cheats: Vec<Vec<DecodedCheat>>,
    pub boot: bool,
    pub enabled: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct CheatData {
    pub note: String,
    pub data: Vec<String>,
    pub options: Option<std::collections::BTreeMap<String, String>>,
}

pub type Cheat = std::collections::BTreeMap<String, std::collections::BTreeMap<String, CheatData>>;

pub fn init(
    device: &mut device::Device,
    cheat_settings: rustc_hash::FxHashMap<String, Option<String>>,
) {
    let cheats = serde_json::from_slice::<Cheat>(include_bytes!("../data/cheats.json"))
        .unwrap()
        .get(&ui::storage::get_game_crc(&device.cart.rom))
        .cloned()
        .unwrap_or_default();

    let re = regex::Regex::new(r"\?+").unwrap();
    for cheat_setting in cheat_settings.iter() {
        if let Some(cheat_data) = cheats.get(cheat_setting.0) {
            let mut option_value = None;
            if let Some(option) = cheat_setting.1 {
                if let Some(found_option_value) = cheat_data
                    .options
                    .as_ref()
                    .cloned()
                    .unwrap_or_default()
                    .get(option)
                {
                    option_value = Some(found_option_value.to_string());
                } else {
                    eprintln!("Cheat option: {option} not found");
                    continue;
                }
            }

            println!(
                "Cheat: {} Option: {}",
                cheat_setting.0,
                cheat_setting.1.clone().unwrap_or("none".to_string())
            );
            let mut decoded_cheat: Vec<DecodedCheat> = vec![];
            for code in cheat_data.data.iter() {
                let mut result = code.clone();
                if let Some(option_value) = option_value.as_ref() {
                    result = re.replace_all(code, option_value).into_owned();
                }
                let mut split = result.split_whitespace();
                let first_part = u32::from_str_radix(split.next().unwrap(), 16).unwrap();
                if let Ok(data) = u16::from_str_radix(split.next().unwrap(), 16) {
                    decoded_cheat.push(DecodedCheat {
                        code_type: (first_part >> 24) as u8,
                        address: first_part & 0x00FFFFFF,
                        data,
                    })
                } else {
                    eprintln!("Could not parse data for: {}", cheat_setting.0);
                };
            }
            if !decoded_cheat.is_empty() {
                device.cheats.cheats.push(decoded_cheat);
            }
        } else {
            eprintln!("Could not find cheat: {}", cheat_setting.0);
        }
    }

    if !device.cheats.cheats.is_empty() {
        device.cheats.enabled = true;
        ui::video::onscreen_message("Cheats enabled", ui::video::MESSAGE_LENGTH_MESSAGE_SHORT);
    }
}

// Cheats touch RDRAM from outside the CPU, so they have to be reconciled with the emulated
// caches. A resident D-cache line otherwise hides the poke from the game (or writes back
// over it), and the I-cache holds pre-decoded instructions, so code patches (the widescreen
// cheats for Ocarina of Time and Majora's Mask are all code patches) would keep running the
// old opcode until the line happened to be evicted.
fn poke_byte(device: &mut device::Device, address: u32, value: u8) {
    let address = address as u64;

    let dline = device::cache::dcache_line(address);
    if device::cache::dcache_hit(device, dline, address) {
        let shift = 8 * (3 - (address & 3));
        let word = &mut device.memory.dcache[dline].words[((address >> 2) & 3) as usize];
        *word = (*word & !(0xFFu32 << shift)) | ((value as u32) << shift);
        device.memory.dcache[dline].dirty = true;
    }

    let iline = device::cache::icache_line(address);
    if device::cache::icache_hit(device, iline, address) {
        device.memory.icache[iline].valid = false;
    }

    *device
        .rdram
        .mem
        .get_mut(address as usize ^ device.byte_swap)
        .unwrap_or(&mut 0) = value;
}

// Conditional codes have to read through the D-cache too, otherwise they test a stale
// RDRAM byte while the game is working out of a dirty cache line.
fn peek_byte(device: &device::Device, address: u32) -> u8 {
    let address = address as u64;

    let dline = device::cache::dcache_line(address);
    if device::cache::dcache_hit(device, dline, address) {
        let shift = 8 * (3 - (address & 3));
        return (device.memory.dcache[dline].words[((address >> 2) & 3) as usize] >> shift) as u8;
    }

    *device
        .rdram
        .mem
        .get(address as usize ^ device.byte_swap)
        .unwrap_or(&0)
}

fn write_byte(device: &mut device::Device, cheat_line: &DecodedCheat) {
    poke_byte(device, cheat_line.address, cheat_line.data as u8);
}

fn write_half(device: &mut device::Device, cheat_line: &DecodedCheat) {
    poke_byte(device, cheat_line.address, (cheat_line.data >> 8) as u8);
    poke_byte(device, cheat_line.address + 1, cheat_line.data as u8);
}

fn equal_byte(device: &device::Device, cheat_line: &DecodedCheat) -> bool {
    peek_byte(device, cheat_line.address) == cheat_line.data as u8
}

fn equal_half(device: &device::Device, cheat_line: &DecodedCheat) -> bool {
    peek_byte(device, cheat_line.address) == (cheat_line.data >> 8) as u8
        && peek_byte(device, cheat_line.address + 1) == cheat_line.data as u8
}

pub fn execute_cheats(device: &mut device::Device) {
    // Take the list so the frame loop doesn't clone every decoded cheat 60 times a second.
    let cheats = std::mem::take(&mut device.cheats.cheats);
    for cheat in cheats.iter() {
        execute_cheat(device, cheat);
    }
    device.cheats.cheats = cheats;
    device.cheats.boot = false;
}

fn execute_cheat(device: &mut device::Device, cheat: &[DecodedCheat]) {
    let mut valid = true;
    let mut cheat_iter = cheat.iter().peekable();
    while let Some(cheat_line) = cheat_iter.next() {
        match cheat_line.code_type {
            0x50 => {
                if valid && let Some(compressed_cheat) = cheat_iter.peek() {
                    let count = (cheat_line.address & 0xFF00) >> 8;
                    let offset = cheat_line.address & 0xFF;
                    let expanded_cheat: Vec<DecodedCheat> = (0..count)
                        .map(|i| DecodedCheat {
                            code_type: compressed_cheat.code_type,
                            address: compressed_cheat.address + (i * offset),
                            data: compressed_cheat
                                .data
                                .wrapping_add((i as i16).wrapping_mul(cheat_line.data as i16) as u16),
                        })
                        .collect();
                    execute_cheat(device, &expanded_cheat);
                }
                valid = false; // skip the next line
            }
            0x80 | 0xA0 => {
                if valid {
                    write_byte(device, cheat_line);
                }
                valid = true;
            }
            0x81 | 0xA1 => {
                if valid {
                    write_half(device, cheat_line);
                }
                valid = true;
            }
            0xF0 => {
                if device.cheats.boot && valid {
                    write_byte(device, cheat_line);
                }
                valid = true;
            }
            0xF1 => {
                if device.cheats.boot && valid {
                    write_half(device, cheat_line);
                }
                valid = true;
            }
            0xD0 => {
                valid = equal_byte(device, cheat_line);
            }
            0xD1 => {
                valid = equal_half(device, cheat_line);
            }
            0xD2 => {
                valid = !equal_byte(device, cheat_line);
            }
            0xD3 => {
                valid = !equal_half(device, cheat_line);
            }
            0x88 | 0x89 | 0xD8 | 0xD9 | 0xDA | 0xDB => { /* GS button not emulated */ }
            _ => eprintln!("Unknown cheat code type: {:X}", cheat_line.code_type),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::cache;

    // A cheat poke has to reach the CPU, not just RDRAM. These cover the two ways the
    // emulated caches used to swallow one: a resident D-cache line serving (and later
    // writing back) the old value, and an I-cache line holding the old pre-decoded
    // instruction, which is exactly what the Zelda widescreen cheats patch.
    // The cache lines are seeded by hand so the test needs no mapped memory or GPU.
    fn test_device() -> Box<device::Device> {
        let mut device = device::Device::new(false);
        device.rdram.mem = vec![0; 0x400000];
        device
    }

    #[test]
    fn poke_updates_a_resident_dcache_line() {
        let mut device = test_device();
        let address: u64 = 0x1000;
        let line = cache::dcache_line(address);

        device.memory.dcache[line].valid = true;
        device.memory.dcache[line].tag = (address & !0xFFF) as u32;
        device.memory.dcache[line].words = [0x1111_1111; 4];

        poke_byte(&mut device, address as u32, 0xAB);

        assert_eq!(peek_byte(&device, address as u32), 0xAB);
        assert_eq!(
            device.memory.dcache[line].words[0], 0xAB11_1111,
            "D-cache line still holds the pre-cheat byte"
        );
        assert!(
            device.memory.dcache[line].dirty,
            "clean line would be dropped and the poke lost"
        );
    }

    #[test]
    fn peek_prefers_the_dcache_over_stale_rdram() {
        let mut device = test_device();
        let address: u64 = 0x1004;
        let line = cache::dcache_line(address);

        device.memory.dcache[line].valid = true;
        device.memory.dcache[line].tag = (address & !0xFFF) as u32;
        device.memory.dcache[line].words[1] = 0x00CD_0000;

        // RDRAM still has the old value the game has not written back yet
        assert_eq!(peek_byte(&device, address as u32 + 1), 0xCD);
    }

    #[test]
    fn poke_drops_a_stale_icache_line() {
        let mut device = test_device();
        let address: u64 = 0x2000;
        let line = cache::icache_line(address);

        device.memory.icache[line].valid = true;
        device.memory.icache[line].tag = (address & !0xFFF) as u32;

        poke_byte(&mut device, address as u32, 0x3C);

        assert!(
            !device.memory.icache[line].valid,
            "patched instruction would keep executing from the I-cache"
        );
    }

    #[test]
    fn poke_falls_back_to_rdram_when_uncached() {
        let mut device = test_device();
        poke_byte(&mut device, 0x3000, 0x7F);
        assert_eq!(peek_byte(&device, 0x3000), 0x7F);
    }
}
