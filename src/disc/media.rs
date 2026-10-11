use serde::{Deserialize, Serialize};

/// Optical media profiles supported for burning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscMedia {
    Cd,
    MDiscDvd,
    Dvd5,
    Dvd9,
    Bd25,
    Bd50,
    Bd100,
}

impl DiscMedia {
    pub fn all() -> &'static [DiscMedia] {
        &[
            DiscMedia::Cd,
            DiscMedia::MDiscDvd,
            DiscMedia::Dvd5,
            DiscMedia::Dvd9,
            DiscMedia::Bd25,
            DiscMedia::Bd50,
            DiscMedia::Bd100,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            DiscMedia::Cd => "CD (~700 MB)",
            DiscMedia::MDiscDvd => "M-DISC DVD (~4.7 GB)",
            DiscMedia::Dvd5 => "DVD-5 (~4.7 GB)",
            DiscMedia::Dvd9 => "DVD-9 (~8.5 GB)",
            DiscMedia::Bd25 => "Blu-ray 25 GB",
            DiscMedia::Bd50 => "Blu-ray 50 GB",
            DiscMedia::Bd100 => "Blu-ray 100 GB",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            DiscMedia::Cd => "CD",
            DiscMedia::MDiscDvd => "M-DISC DVD",
            DiscMedia::Dvd5 => "DVD-5",
            DiscMedia::Dvd9 => "DVD-9",
            DiscMedia::Bd25 => "BD-25",
            DiscMedia::Bd50 => "BD-50",
            DiscMedia::Bd100 => "BD-100",
        }
    }

    pub fn default_for_new() -> Self {
        DiscMedia::Bd25
    }

    /// Usable capacity without BD Defect Management spare (default burn path).
    pub fn capacity_bytes(self) -> u64 {
        self.capacity_for_burn(false)
    }

    /// Usable capacity for planning/burning.
    ///
    /// When `defect_management` is set on Blu-ray, spare area shrinks free space
    /// (observed ~1.3 GiB on BD-50 with xorriso `-format as_needed`).
    pub fn capacity_for_burn(self, defect_management: bool) -> u64 {
        match self {
            // 80-minute data CD is 360_000 × 2048 = 737_280_000 bytes. Leave ~2%.
            DiscMedia::Cd => 722_534_400,
            // 4,700,000,000 decimal manufacturer rating ≈ 4.37 GiB; leave ~2% margin.
            // M-DISC DVD is the same capacity as DVD-5.
            DiscMedia::MDiscDvd | DiscMedia::Dvd5 => 4_377_000_000,
            DiscMedia::Dvd9 => 7_925_000_000,
            // BD manufacturer "25/50/100 GB" is optimistic vs real blank free space
            // after filesystem overhead; leave ~1.5% under typical unformatted free.
            DiscMedia::Bd25 => {
                if defect_management {
                    23_950_000_000
                } else {
                    24_650_000_000
                }
            }
            DiscMedia::Bd50 => {
                if defect_management {
                    47_900_000_000
                } else {
                    49_300_000_000
                }
            }
            DiscMedia::Bd100 => {
                if defect_management {
                    95_800_000_000
                } else {
                    98_600_000_000
                }
            }
        }
    }

    pub fn is_bluray(self) -> bool {
        matches!(self, DiscMedia::Bd25 | DiscMedia::Bd50 | DiscMedia::Bd100)
    }

    /// M-DISC DVD is write-once. Blu-ray M-DISC uses the existing BD profiles.
    pub fn is_mdisc(self) -> bool {
        matches!(self, DiscMedia::MDiscDvd)
    }

    /// New discs of this profile start with Blank RW on.
    /// M-DISC DVD starts off: the disc cannot be erased for reuse.
    pub fn blank_default(self) -> bool {
        !self.is_mdisc()
    }

    /// CD-RW, DVD±RW, and BD-RE can take a full new image. M-DISC DVD cannot.
    pub fn allows_reburn(self) -> bool {
        !self.is_mdisc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_capacity_is_two_percent_under_80_minute_data() {
        let raw = 360_000u64 * 2048;
        assert_eq!(raw, 737_280_000);
        assert_eq!(DiscMedia::Cd.capacity_bytes(), raw * 98 / 100);
    }

    #[test]
    fn mdisc_dvd_matches_dvd5_and_is_not_bluray() {
        assert_eq!(
            DiscMedia::MDiscDvd.capacity_bytes(),
            DiscMedia::Dvd5.capacity_bytes()
        );
        assert!(!DiscMedia::Cd.is_bluray());
        assert!(!DiscMedia::MDiscDvd.is_bluray());
        assert!(!DiscMedia::MDiscDvd.blank_default());
        assert!(DiscMedia::Cd.blank_default());
        assert!(!DiscMedia::MDiscDvd.allows_reburn());
        assert!(DiscMedia::Cd.allows_reburn());
        assert!(DiscMedia::Bd25.allows_reburn());
    }
}
