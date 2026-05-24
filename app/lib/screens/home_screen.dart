import 'package:flutter/material.dart';
import 'package:file_picker/file_picker.dart';
import 'package:path/path.dart' as p;
import '../services/secure_clipboard.dart';
import '../src/rust/api/clipper.dart';
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
      final text = await copyToLocal(id: id);
      await setSecureClipboardText(text);
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

  Future<void> _uploadFile() async {
    final result = await FilePicker.pickFiles();
    if (result == null || result.files.isEmpty) return;

    final path = result.files.single.path;
    if (path == null) return;

    try {
      await uploadFile(filePath: path);
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
    final dir = await FilePicker.getDirectoryPath();
    if (dir == null) return;

    final targetPath = p.join(dir, safeDownloadFilename(filename));
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
    if (items.isEmpty) {
      return const AppStatus(
        icon: Icons.content_paste_off,
        title: 'No clipboard items yet',
      );
    }

    return ListView.builder(
      padding: const EdgeInsets.all(8),
      itemCount: items.length,
      itemBuilder: (context, index) {
        final item = items[index];
        final mime = item.mimeType.toLowerCase().split(';').first.trim();
        final isText = mime.startsWith('text/');
        return SyncListTileCard(
          title: Text(
            item.text,
            maxLines: 3,
            overflow: TextOverflow.ellipsis,
            style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
          ),
          subtitle: Text(
            '${item.mimeType} - ${formatRelativeTimestamp(item.createdAt)}',
            style: const TextStyle(fontSize: 11, color: Colors.white38),
          ),
          trailing: isText
              ? IconButton(
                  icon: const Icon(Icons.copy, size: 20),
                  onPressed: () => _copyToClipboard(item.id),
                  tooltip: 'Copy to clipboard',
                )
              : const Icon(Icons.image, size: 20, color: Colors.white54),
          onTap: isText ? () => _copyToClipboard(item.id) : null,
        );
      },
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
