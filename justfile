# Run every check that CI runs
pre-commit: format-check lint test

# Format the sources
format:
    cargo fmt

# Check that the sources are formatted
format-check:
    cargo fmt --check

# Lint the sources
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Run every rule's tests
test:
    cargo nextest run --workspace --all-features --config-file .config/nextest.toml

# Check this repository with the rules in it
#
# The stiles binary must be the release `Cargo.toml` pins, because a
# plugin built against one stiles is refused by any other.
check-self stiles="stiles":
    {{ stiles }} check .

# Assemble an archive of prebuilt lints for one stiles
package-lints rev stiles:
    #!/usr/bin/env -S bash -euo pipefail
    # Stiles looks for an archive named for the commit it pins and for the
    # tag of the binary asking. `stiles abi` prints that tag, so the name
    # comes from the stiles that will load these libraries rather than from
    # anything written here.
    rev="{{ rev }}"
    stiles="{{ stiles }}"

    # The libraries are built with the toolchain this repository pins.
    # Stiles loads a library regardless of which rustc built it, so the
    # choice is this repository's own.
    cargo build --release --locked --workspace

    name="${rev}-$("${stiles}" abi)"
    rm -rf "dist/${name}"
    mkdir -p "dist/${name}"

    # A publisher ships one library per rule, flat, and stiles unpacks only
    # the regular files at the archive's root.
    shopt -s nullglob
    libraries=(target/release/*.so target/release/*.dylib)
    if [ "${#libraries[@]}" -eq 0 ]; then
        echo "the workspace built no dynamic library" >&2
        exit 1
    fi
    cp "${libraries[@]}" "dist/${name}/"

    # The names go in bare. Stiles unpacks only the entries at the archive's
    # root, and `tar -C dir .` writes them all with a `./` prefix, which is a
    # second path component and so is skipped.
    names=()
    for library in "${libraries[@]}"; do
        names+=("$(basename "${library}")")
    done

    tar -czf "dist/${name}.tar.gz" -C "dist/${name}" "${names[@]}"
    rm -rf "dist/${name}"
    (cd dist && shasum -a 256 "${name}.tar.gz" > "${name}.tar.gz.sha256")

# Download the stiles release that every rule is built against, for one target
#
# The binary lands at `target/stiles/stiles`. CI runs it, so that the rules
# are checked and packaged by the same stiles that they build against.
fetch-stiles target:
    #!/usr/bin/env -S bash -euo pipefail
    version="$({{ quote(just_executable()) }} --justfile {{ quote(justfile()) }} pin)"
    name="stiles-${version}-{{ target }}"
    rm -rf target/stiles
    mkdir -p target/stiles
    gh release download "stiles@${version}" --repo aonyx-ai/stiles \
        --dir target/stiles --pattern "${name}.tar.gz" --pattern "${name}.tar.gz.sha256"
    (cd target/stiles && shasum -a 256 -c "${name}.tar.gz.sha256")
    tar -xzf "target/stiles/${name}.tar.gz" -C target/stiles --strip-components 1 "${name}/stiles"
    target/stiles/stiles --version

# Print the stiles release every rule is built against
pin:
    @grep -m1 '^stiles-plugin = ' Cargo.toml | sed 's/.*"=\([^"]*\)".*/\1/'
