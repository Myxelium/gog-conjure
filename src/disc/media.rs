use serde::{Deserialize, Serialize};

/// Optical media profiles supported for burning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscMedia {
    Dvd5,
    Dvd9,
    Bd25,
    Bd50,
    Bd100,
}

impl DiscMedia {
    pub fn all() -> &'static [DiscMedia] {
        &[
            DiscMedia::Dvd5,
            DiscMedia::Dvd9,
            DiscMedia::Bd25,
            DiscMedia::Bd50,
            DiscMedia::Bd100,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            DiscMedia::Dvd5 => "DVD-5 (~4.7 GB)",
            DiscMedia::Dvd9 => "DVD-9 (~8.5 GB)",
            DiscMedia::Bd25 => "Blu-ray 25 GB",
            DiscMedia::Bd50 => "Blu-ray 50 GB",
            DiscMedia::Bd100 => "Blu-ray 100 GB",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
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
            // 4,700,000,000 decimal manufacturer rating ≈ 4.37 GiB; leave ~2% margin.
            DiscMedia::Dvd5 => 4_377_000_000,
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
}
