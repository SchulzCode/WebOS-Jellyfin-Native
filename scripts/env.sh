# Shared settings for the dev scripts. Copy config.example.env to config.env (git-ignored) and fill it in.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[ -f "$ROOT/config.env" ] && . "$ROOT/config.env"
: "${TV_IP:?set TV_IP in config.env - the address of the TV}"
: "${TV_SSH_KEY:=$HOME/.ssh/id_rsa}"
: "${PC_HTTP_PORT:=1759}"
# this machine's address as the TV sees it (the TV downloads the package from here)
: "${PC_IP:=$(ip route get "$TV_IP" 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')}"
SSH_OPTS="-i $TV_SSH_KEY -o BatchMode=yes -o ConnectTimeout=8 -o LogLevel=ERROR"
[ -n "${TV_KNOWN_HOSTS:-}" ] && SSH_OPTS="$SSH_OPTS -o UserKnownHostsFile=$TV_KNOWN_HOSTS"
# root shell on the TV (rooted TV with SSH enabled in Homebrew Channel); binary-safe
tv() { ssh $SSH_OPTS "root@$TV_IP" "$@"; }
# the same for luna-send, which silently prints nothing over SSH without a pseudo-terminal
tvl() { ssh -tt $SSH_OPTS "root@$TV_IP" "$@"; }
