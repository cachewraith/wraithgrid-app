//! Mods and shared config for the GPUI app (#166, ADR 0005), ported from the Electron main
//! process: claude plugins listed, installed and switched per account through each account's
//! own `claude plugin` command (`mods.ts` and the mod handlers of `ipc.ts`), the claudemod.com
//! community list (`mod-community.ts`), and one shared brain per provider linked into every
//! account's config dir (`shared-config.ts`). A plain library: accounts, the home dir, the
//! claude binary and its env come in as parameters; claude and the network are injected so
//! tests run neither.

pub mod mod_community;
pub mod mod_manager;
pub mod mods;
pub mod shared_config;
mod util;

pub use mod_community::{
    ureq_fetch, CommunityCatalog, CommunityEntry, CommunityMods, Fetch, FetchRequest,
    FetchResponse, COMMUNITY_URL,
};
pub use mod_manager::{
    AccountMods, FailedAccount, MarketplaceList, ModChange, ModDeps, ModManager,
};
pub use mods::{
    CatalogMod, ClaudeCli, ClaudeRunner, Env, Marketplace, MarketplaceInfo, ModInfo, RunOptions,
};
pub use shared_config::{
    ItemKind, ItemResult, LinkOutcome, ShareSpec, SharedBrains, SharedItem, SharedMode,
    SharedReportEntry,
};

/// An account as these features see it: what `Config['accounts']` holds that they read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    /// What logs and reports call it.
    pub name: String,
    /// The agent CLI: `claude`, `codex`, `gemini`, …
    pub cli: String,
    /// As saved in the config: may start with `~`.
    pub config_dir: String,
}
