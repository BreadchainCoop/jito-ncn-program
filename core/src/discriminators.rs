#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discriminators {
    // Configs
    Config = 0x01,
    VaultRegistry = 0x02,
    OperatorRegistry = 0x03,

    // Snapshots
    WeightTable = 0x10,
    Snapshot = 0x11,
    OperatorSnapshot = 0x12,
}
