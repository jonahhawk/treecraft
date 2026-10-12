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
| 2026-10-11 | Microsoft Learn: FILE_ID_EXTD_DIR_INFO | https://learn.microsoft.com/windows/win32/api/winbase/ns-winbase-file_id_extd_dir_info | File IDs for hard-link dedup |
| 2026-10-11 | Microsoft Learn: GetVolumePathNameW, GetVolumeInformationW | https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationw | Detecting NTFS before relying on file IDs |
| 2026-10-11 | Microsoft Learn: SetThreadPriority | https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadpriority | Below-normal scan threads in the GUI |
| 2026-10-11 | Microsoft Learn: FSCTL_SET_SPARSE, FSCTL_SET_COMPRESSION | https://learn.microsoft.com/windows/win32/api/winioctl/ni-winioctl-fsctl_set_sparse | Creating sparse and compressed files in the edge-case tests |
| 2026-10-11 | Microsoft Learn: NtOpenFile, OBJECT_ATTRIBUTES | https://learn.microsoft.com/windows/win32/api/winternl/nf-winternl-ntopenfile | Parent-relative open experiment (`rel_open`) |
| 2026-10-11 | Microsoft Learn: FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER | https://learn.microsoft.com/windows/win32/api/winioctl/ni-winioctl-fsctl_get_ntfs_volume_data | MFT location, record and cluster size |
| 2026-10-11 | Microsoft Learn: GetVolumeNameForVolumeMountPointW | https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getvolumenameforvolumemountpointw | Opening the volume device of a scan root |
| 2026-10-11 | Microsoft Learn: GetTokenInformation, TOKEN_ELEVATION | https://learn.microsoft.com/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation | Detecting administrator rights |
| 2026-10-11 | Microsoft Learn: ShellExecuteExW, SHELLEXECUTEINFOW ("runas" verb) | https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw | "Run as admin" relaunch |
| 2026-10-11 | Microsoft Learn: CommandLineToArgvW (argument quoting rules) | https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-commandlinetoargvw | Quoting the folder passed to the relaunched process |
| 2026-10-11 | Linux-NTFS project documentation: FILE records, attributes, fixups, runlists, attribute lists, boot sector | https://flatcap.github.io/linux-ntfs/ntfs/ | `tc-ntfs` on-disk layouts |
