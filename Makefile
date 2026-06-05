# armoury-tui — convenience targets. The real installer is ./install.sh.
MANIFEST := rust/Cargo.toml

.PHONY: build install system-install uninstall run probe test fmt clean

build:        ## compile the release binary
	cargo build --release --manifest-path $(MANIFEST)

install:      ## build + install for the current user (~/.local)
	./install.sh

system-install: ## build + install system-wide (/usr/local, uses sudo)
	./install.sh --system

uninstall:    ## remove the installed binary + desktop entry
	./install.sh --uninstall

run:          ## run from source (dev)
	cargo run --manifest-path $(MANIFEST)

probe:        ## print detected hardware and exit
	cargo run --release --manifest-path $(MANIFEST) -- --probe

test:         ## run the unit tests
	cargo test --manifest-path $(MANIFEST)

fmt:          ## format the Rust sources
	cargo fmt --manifest-path $(MANIFEST)

clean:        ## remove build artifacts
	cargo clean --manifest-path $(MANIFEST)
