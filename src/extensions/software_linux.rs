use std::path::PathBuf;

use super::ExtensionId;

pub fn default_binary_path(id: ExtensionId) -> &'static str {
    match id {
        ExtensionId::Ttyd => "/usr/bin/ttyd",
        ExtensionId::Gostc => "/usr/bin/gostc",
        ExtensionId::Easytier => "/usr/bin/easytier-core",
        ExtensionId::Frpc => "/usr/bin/frpc",
    }
}

pub fn binary_path(id: ExtensionId) -> PathBuf {
    #[cfg(feature = "vim4")]
    {
        let name = match id {
            ExtensionId::Gostc => "gostc",
            ExtensionId::Easytier => "easytier-core",
            ExtensionId::Frpc => "frpc",
            ExtensionId::Ttyd => "ttyd",
        };
        return PathBuf::from("/opt/vim4-onekvm/bin").join(name);
    }
    #[cfg(not(feature = "vim4"))]
    PathBuf::from(default_binary_path(id))
}

pub fn default_ttyd_shell() -> &'static str {
    "/bin/bash"
}
