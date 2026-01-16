PREFIX ?= /usr/local
BINDIR = $(PREFIX)/bin
MANDIR = $(PREFIX)/share/man/man1

all: build

build:
	cargo build --release

install: build
	install -d $(DESTDIR)$(BINDIR)
	install -m 755 target/release/xpose $(DESTDIR)$(BINDIR)/xpose
	install -d $(DESTDIR)$(MANDIR)
	install -m 644 xpose.1 $(DESTDIR)$(MANDIR)/xpose.1
	gzip -f $(DESTDIR)$(MANDIR)/xpose.1

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/xpose
	rm -f $(DESTDIR)$(MANDIR)/xpose.1.gz

clean:
	cargo clean

.PHONY: all build install uninstall clean
