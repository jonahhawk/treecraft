# Sources

Clean-room provenance log. Every external source consulted while writing the specs or the
implementation is listed here, with the date it was accessed. See PLAN.md §0 for what is allowed.

Allowed: public product pages, public user manuals and help pages, published reviews,
screenshots, observing a product's behaviour from the outside, and platform documentation
(Microsoft Learn, NTFS format write-ups).

Not allowed: decompiled or disassembled code, copied UI text, icons, help text or report templates.

| Date | Source | URL | Used for |
|---|---|---|---|
| 2026-10-11 | Microsoft Learn: GetFileInformationByHandleEx, FILE_ID_BOTH_DIR_INFO | https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex | Win32 directory lister |
| 2026-10-11 | Microsoft Learn: reparse point tags and IsReparseTagNameSurrogate | https://learn.microsoft.com/windows/win32/fileio/reparse-point-tags | Deciding which reparse points are links |
| 2026-10-11 | Microsoft Learn: file attribute constants (RECALL_ON_DATA_ACCESS etc.) | https://learn.microsoft.com/windows/win32/fileio/file-attribute-constants | Cloud placeholder detection |
| 2026-10-11 | Microsoft Learn: maximum path length limitation | https://learn.microsoft.com/windows/win32/fileio/maximum-file-path-limitation | `\\?\` long-path handling |
| 2026-10-11 | Bruls, Huizing, van Wijk, "Squarified Treemaps" (2000) | https://www.win.tue.nl/~vanwijk/stm.pdf | Treemap layout algorithm |
