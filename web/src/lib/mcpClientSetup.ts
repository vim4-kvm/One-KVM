export function codexInstallCommand(endpoint: string, key: string): string {
  return `codex mcp remove one-kvm 2>/dev/null; cat >> ~/.codex/config.toml <<'EOF'

[mcp_servers.one-kvm]
url = "${endpoint}"
http_headers = { Authorization = "Bearer ${key}" }
EOF
`
}
