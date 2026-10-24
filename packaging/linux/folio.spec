# folio 的 rpm spec。@VERSION@ 由 workflow 替换。
#
# **这里故意不写 `Requires:`**。rpm 打包默认跑 `find-requires`：它扫装进去的 ELF，把
# NEEDED 里的 `.so` 名字换成提供它们的包。手写一份依赖表等于在这条自动链旁边再立一个
# 会腐烂的真相 —— 与 deb 那一头用 `dpkg-shlibdeps` 现算是同一个理由。
#
# `%{?dist}` 由 rpmbuild 自己加，所以 Release 只写 1。

Name:           folio
Version:        @VERSION@
Release:        1
Summary:        Native GPU ebook reader with a layout engine written from scratch
License:        MIT
URL:            https://github.com/panzhifu/folio
Source0:        folio-@VERSION@-linux-x86_64.tar.xz

%description
Read EPUB, PDF, MOBI/AZW3, DOCX/ODT, DjVu and comic archives with a layout
engine written from scratch: first-line indent, centred titles, per-run
styles, inline and block images, CJK line breaking. No embedded browser.

%prep
%autosetup -n folio-@VERSION@

%build
# 二进制是 cargo 在 workflow 里先编好的，这一格没有可编的东西。留着空 %build 是为了
# 让 rpmbuild 不去猜该怎么编。

%install
install -d -m 755 %{buildroot}%{_bindir}
install -d -m 755 %{buildroot}%{_datadir}/applications
install -d -m 755 %{buildroot}%{_datadir}/icons/hicolor/192x192/apps
install -d -m 755 %{buildroot}%{_docdir}/folio
install -m 755 bin/folio %{buildroot}%{_bindir}/folio
install -m 644 share/applications/folio.desktop %{buildroot}%{_datadir}/applications/
install -m 644 share/icons/hicolor/192x192/apps/folio.png \
  %{buildroot}%{_datadir}/icons/hicolor/192x192/apps/folio.png
install -m 644 THIRD-PARTY-NOTICES.txt %{buildroot}%{_docdir}/folio/
install -m 644 LICENSE %{buildroot}%{_docdir}/folio/ 2>/dev/null || :

%files
%{_bindir}/folio
%{_datadir}/applications/folio.desktop
%{_datadir}/icons/hicolor/192x192/apps/folio.png
%doc %{_docdir}/folio/THIRD-PARTY-NOTICES.txt

%changelog
* Sat Oct 24 2026 panzhifu <noke601508@outlook.com> - @VERSION@-1
- First packaged release with four-platform artifacts.
