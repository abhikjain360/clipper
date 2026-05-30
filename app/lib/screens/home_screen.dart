import 'package:flutter/material.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;
import '../services/secure_clipboard.dart';
import '../src/rust/api/clipper.dart';
import '../utils/browser_download.dart' as browser_download;
import '../utils/file_helpers.dart';
import '../utils/formatters.dart';
import '../widgets/app_status.dart';
import '../widgets/clipper_brand.dart';
import '../widgets/connection_badge.dart';
import '../widgets/loading_icon_button.dart';
import '../widgets/sync_list_tile_card.dart';

class HomeScreen extends StatefulWidget {
  final BridgeAppState state;

  const HomeScreen({super.key, required this.state});

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  bool _refreshing = false;
  bool _addingClipboard = false;

  bool get _supportsManualClipboardImport =>
      kIsWeb || defaultTargetPlatform == TargetPlatform.android;

  Future<void> _refresh() async {
    setState(() => _refreshing = true);
    try {
      await refresh();
    } catch (e) {
      _showError(e.toString());
    } finally {
      if (mounted) setState(() => _refreshing = false);
    }
  }

  Future<void> _handleLogout() async {
    try {
      await logout();
    } catch (_) {}
    // State change will be picked up by AppRoot's watcher
  }

  Future<void> _copyToClipboard(String id) async {
    try {
      final payload = await clipboardPayload(id: id);
      await setSecureClipboardEntry(
        DeviceClipboardEntry(
          mimeType: payload.mimeType,
          bytes: payload.bytes,
          text: payload.text,
        ),
      );
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(
            content: Text('Copied to clipboard'),
            duration: Duration(seconds: 1),
          ),
        );
      }
    } catch (e) {
      _showError(e.toString());
    }
  }

  Future<void> _addCurrentClipboard() async {
    setState(() => _addingClipboard = true);
    try {
      final entry = await readDeviceClipboardEntry();
      if (entry == null) {
        _showError('Clipboard is empty or unavailable');
        return;
      }
      await sendClipboardPayload(mimeType: entry.mimeType, bytes: entry.bytes);
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(const SnackBar(content: Text('Clipboard added')));
      }
    } catch (e) {
      _showError(e.toString());
    } finally {
      if (mounted) setState(() => _addingClipboard = false);
    }
  }

  Future<void> _uploadFile() async {
    final result = await FilePicker.pickFiles(withData: kIsWeb);
    if (result == null || result.files.isEmpty) return;

    final file = result.files.single;

    try {
      if (kIsWeb) {
        final bytes = file.bytes;
        if (bytes == null) {
          _showError('Could not read selected file');
          return;
        }
        await uploadFileBytes(filename: file.name, mimeType: '', bytes: bytes);
      } else {
        final path = file.path;
        if (path == null) return;
        await uploadFile(filePath: path);
      }
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(const SnackBar(content: Text('File uploaded')));
      }
    } catch (e) {
      _showError(e.toString());
    }
  }

  Future<void> _downloadFile(String fileId, String filename) async {
    final downloadName = safeDownloadFilename(filename);

    if (kIsWeb) {
      try {
        final bytes = await downloadFileBytes(fileId: fileId);
        await browser_download.downloadBytes(downloadName, bytes);
        if (mounted) {
          ScaffoldMessenger.of(
            context,
          ).showSnackBar(const SnackBar(content: Text('Download started')));
        }
      } catch (e) {
        _showError(e.toString());
      }
      return;
    }

    final dir = await FilePicker.getDirectoryPath();
    if (dir == null) return;

    final targetPath = p.join(dir, downloadName);
    try {
      await downloadFile(fileId: fileId, targetPath: targetPath);
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text('Downloaded to $targetPath')));
      }
    } catch (e) {
      _showError(e.toString());
    }
  }

  Future<void> _deleteFileConfirm(String fileId, String filename) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Delete file?'),
        content: Text('Delete "$filename" from server?'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text(
              'Delete',
              style: TextStyle(color: Colors.redAccent),
            ),
          ),
        ],
      ),
    );
    if (confirmed != true) return;

    try {
      await deleteFile(fileId: fileId);
    } catch (e) {
      _showError(e.toString());
    }
  }

  void _showError(String msg) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(msg), backgroundColor: Colors.redAccent),
    );
  }

  @override
  Widget build(BuildContext context) {
    final state = widget.state;

    return DefaultTabController(
      length: 2,
      child: Scaffold(
        appBar: AppBar(
          title: ClipperAppTitle(
            trailing: ConnectionBadge(status: state.connectionStatus),
          ),
          actions: [
            LoadingIconButton(
              loading: _refreshing,
              icon: Icons.refresh,
              tooltip: 'Refresh',
              onPressed: _refresh,
            ),
            IconButton(
              icon: const Icon(Icons.logout),
              onPressed: _handleLogout,
              tooltip: 'Logout',
            ),
          ],
          bottom: const TabBar(
            tabs: [
              Tab(icon: Icon(Icons.content_paste), text: 'Clipboard'),
              Tab(icon: Icon(Icons.folder), text: 'Files'),
            ],
          ),
        ),
        body: TabBarView(
          children: [_buildClipboardTab(state), _buildFilesTab(state)],
        ),
      ),
    );
  }

  Widget _buildClipboardTab(BridgeAppState state) {
    final items = state.clipboardItems;
    return Column(
      children: [
        if (_supportsManualClipboardImport)
          Padding(
            padding: const EdgeInsets.all(8),
            child: SizedBox(
              width: double.infinity,
              child: OutlinedButton.icon(
                onPressed: _addingClipboard ? null : _addCurrentClipboard,
                icon: _addingClipboard
                    ? const SizedBox(
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.add),
                label: const Text('Add Current Clipboard'),
              ),
            ),
          ),
        if (items.isEmpty)
          const Expanded(
            child: AppStatus(
              icon: Icons.content_paste_off,
              title: 'No clipboard items yet',
            ),
          )
        else
          Expanded(
            child: ListView.builder(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              itemCount: items.length,
              itemBuilder: (context, index) {
                final item = items[index];
                final mime = item.mimeType
                    .toLowerCase()
                    .split(';')
                    .first
                    .trim();
                final isText = mime.startsWith('text/');
                final isImage = mime.startsWith('image/');
                final canCopy = isText || isImage;
                return SyncListTileCard(
                  leading: isImage ? _ClipboardImagePreview(id: item.id) : null,
                  title: Text(
                    item.text,
                    maxLines: isImage ? 1 : 3,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontFamily: isText ? 'monospace' : null,
                      fontSize: 13,
                    ),
                  ),
                  subtitle: Text(
                    '${item.mimeType} - ${formatRelativeTimestamp(item.createdAt)}',
                    style: const TextStyle(fontSize: 11, color: Colors.white38),
                  ),
                  trailing: canCopy
                      ? IconButton(
                          icon: const Icon(Icons.copy, size: 20),
                          onPressed: () => _copyToClipboard(item.id),
                          tooltip: 'Copy to clipboard',
                        )
                      : const Icon(Icons.insert_drive_file, size: 20),
                  onTap: canCopy ? () => _copyToClipboard(item.id) : null,
                );
              },
            ),
          ),
      ],
    );
  }

  Widget _buildFilesTab(BridgeAppState state) {
    final files = state.files;

    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.all(8),
          child: SizedBox(
            width: double.infinity,
            child: OutlinedButton.icon(
              onPressed: _uploadFile,
              icon: const Icon(Icons.upload_file),
              label: const Text('Upload File'),
            ),
          ),
        ),
        if (files.isEmpty)
          const Expanded(
            child: AppStatus(icon: Icons.folder_off, title: 'No files yet'),
          )
        else
          Expanded(
            child: ListView.builder(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              itemCount: files.length,
              itemBuilder: (context, index) {
                final file = files[index];
                return SyncListTileCard(
                  leading: Icon(
                    fileIconForMimeType(file.mimeType),
                    color: Colors.blue,
                  ),
                  title: Text(file.filename),
                  subtitle: Text(
                    '${formatByteSize(file.blobSize)} - ${formatRelativeTimestamp(file.createdAt)}',
                    style: const TextStyle(fontSize: 11, color: Colors.white38),
                  ),
                  trailing: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      IconButton(
                        icon: const Icon(Icons.download, size: 20),
                        onPressed: () => _downloadFile(file.id, file.filename),
                        tooltip: 'Download',
                      ),
                      IconButton(
                        icon: const Icon(
                          Icons.delete,
                          size: 20,
                          color: Colors.redAccent,
                        ),
                        onPressed: () =>
                            _deleteFileConfirm(file.id, file.filename),
                        tooltip: 'Delete',
                      ),
                    ],
                  ),
                );
              },
            ),
          ),
      ],
    );
  }
}

class _ClipboardImagePreview extends StatelessWidget {
  final String id;

  const _ClipboardImagePreview({required this.id});

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<BridgeClipboardPayload>(
      future: clipboardPayload(id: id),
      builder: (context, snapshot) {
        if (snapshot.hasData) {
          return ClipRRect(
            borderRadius: BorderRadius.circular(4),
            child: Image.memory(
              snapshot.data!.bytes,
              width: 44,
              height: 44,
              fit: BoxFit.cover,
              errorBuilder: (context, error, stackTrace) =>
                  const Icon(Icons.image, size: 28),
            ),
          );
        }
        return const SizedBox(
          width: 44,
          height: 44,
          child: Center(
            child: SizedBox(
              width: 18,
              height: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
          ),
        );
      },
    );
  }
}
