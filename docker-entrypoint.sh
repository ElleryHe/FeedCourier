#!/bin/sh
set -eu

if [ "$#" -eq 0 ]; then
    set -- /usr/local/bin/feedcourier
elif [ "${1#-}" != "$1" ]; then
    set -- /usr/local/bin/feedcourier "$@"
fi

# When starting FeedCourier as root (default Docker behavior), ensure DATA_DIR
# exists and is owned by `feedcourier` (10001:10001), then drop privileges via
# `setpriv` (util-linux) while preserving PID 1 for clean signal handling.
# If started with an explicit non-root `--user` (e.g. Kubernetes runAsNonRoot),
# skip ownership changes and exec directly under the caller-provided UID/GID.
if [ "$1" = "/usr/local/bin/feedcourier" ] || [ "$1" = "feedcourier" ]; then
    if [ "$(id -u)" = "0" ]; then
        data_dir="${DATA_DIR:-/data}"
        if [ -z "$(printf '%s' "$data_dir" | tr -d '[:space:]')" ]; then
            data_dir="/data"
        fi

        mkdir -p "$data_dir"
        if ! find -H "$data_dir" \( ! -user feedcourier -o ! -group feedcourier \) \
            -exec chown -h feedcourier:feedcourier {} +; then
            echo "docker-entrypoint: warning: failed to adjust ownership of '$data_dir'; continuing" >&2
        fi

        export HOME="$data_dir"
        exec setpriv --reuid=feedcourier --regid=feedcourier --init-groups -- "$@"
    fi
fi

exec "$@"
