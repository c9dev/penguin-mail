# Finds the accessibility bus launcher and registry daemon, which
# at-spi2-core installs somewhere different on every distribution: Debian,
# Ubuntu and Fedora use /usr/libexec, Arch /usr/lib, openSUSE
# /usr/libexec/at-spi2, and NixOS a store path that only XDG_DATA_DIRS
# leads to. Sourced by the scripts that start a hidden display, after they
# cd to the repository:
#
#   . scripts/at-spi.sh
#   find_at_spi a11y-names.sh     # sets $launcher and $registry, or exits 2
#
# The D-Bus service files name the daemons exactly, so they are read
# first, in every data directory XDG_DATA_DIRS lists. The usual install
# directories come after, for a system that ships the daemons without the
# service files. AT_SPI_BUS_LAUNCHER and AT_SPI_REGISTRYD override both.

# The program a D-Bus service file runs, without its arguments.
at_spi_exec_of() {
    local file=$1 line
    [ -r "$file" ] || return 1
    line=$(grep -m1 '^Exec=' "$file") || return 1
    line=${line#Exec=}
    printf '%s\n' "${line%% *}"
}

# The path of one daemon: its override, then its service file, then the
# usual directories. Prints nothing when none of them has it.
at_spi_find() {
    local name=$1 override=$2 service=$3 dir found
    if [ -n "$override" ]; then
        printf '%s\n' "$override"
        return
    fi
    local IFS=:
    for dir in ${XDG_DATA_DIRS:-/usr/local/share:/usr/share}; do
        found=$(at_spi_exec_of "$dir/dbus-1/$service") || continue
        if [ -x "$found" ]; then
            printf '%s\n' "$found"
            return
        fi
    done
    for dir in /usr/libexec /usr/lib /usr/libexec/at-spi2 \
        /usr/libexec/at-spi2-core /usr/lib/at-spi2-core; do
        if [ -x "$dir/$name" ]; then
            printf '%s\n' "$dir/$name"
            return
        fi
    done
}

# Sets $launcher and $registry, or says what is missing and exits 2, the
# status the scripts use for a missing tool. $1 names the script in the
# message.
find_at_spi() {
    launcher=$(at_spi_find at-spi-bus-launcher "${AT_SPI_BUS_LAUNCHER:-}" \
        services/org.a11y.Bus.service)
    registry=$(at_spi_find at-spi2-registryd "${AT_SPI_REGISTRYD:-}" \
        accessibility-services/org.a11y.atspi.Registry.service)
    if [ ! -x "$launcher" ] || [ ! -x "$registry" ]; then
        echo "$1 needs at-spi2-core installed; set AT_SPI_BUS_LAUNCHER and" \
            "AT_SPI_REGISTRYD if its daemons are somewhere unusual" >&2
        exit 2
    fi
}
