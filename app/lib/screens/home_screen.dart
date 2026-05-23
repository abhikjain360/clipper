import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:file_picker/file_picker.dart';
import '../src/rust/api/clipper.dart';

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
      await Clipboard.setData(ClipboardData(text: text));
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
    final result = await FilePicker.platform.pickFiles();
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
    final dir = await FilePicker.platform.getDirectoryPath();
    if (dir == null) return;

    final targetPath = '$dir/$filename';
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
          title: Row(
            children: [
              const Icon(Icons.content_paste_rounded, color: Colors.blue),
              const SizedBox(width: 8),
              const Text('Clipper'),
              const SizedBox(width: 12),
              _connectionBadge(state.connectionStatus),
            ],
          ),
          actions: [
            IconButton(
              icon: _refreshing
                  ? const SizedBox(
                      width: 20,
                      height: 20,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Icon(Icons.refresh),
              onPressed: _refreshing ? null : _refresh,
              tooltip: 'Refresh',
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

  Widget _connectionBadge(BridgeConnectionStatus status) {
    Color color;
    String label;
    switch (status) {
      case BridgeConnectionStatus.connected:
        color = Colors.green;
        label = 'Connected';
      case BridgeConnectionStatus.connecting:
        color = Colors.orange;
        label = 'Connecting...';
      default:
        color = Colors.red;
        label = 'Disconnected';
    }
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: color.withAlpha(40),
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: color.withAlpha(100)),
      ),
      child: Text(label, style: TextStyle(fontSize: 12, color: color)),
    );
  }

  Widget _buildClipboardTab(BridgeAppState state) {
    final items = state.clipboardItems;
    if (items.isEmpty) {
      return const Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.content_paste_off, size: 64, color: Colors.white24),
            SizedBox(height: 16),
            Text(
              'No clipboard items yet',
              style: TextStyle(color: Colors.white38),
            ),
          ],
        ),
      );
    }

    return ListView.builder(
      padding: const EdgeInsets.all(8),
      itemCount: items.length,
      itemBuilder: (context, index) {
        final item = items[index];
        return Card(
          margin: const EdgeInsets.symmetric(vertical: 4),
          child: ListTile(
            title: Text(
              item.text,
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
            ),
            subtitle: Text(
              _formatTimestamp(item.createdAt),
              style: const TextStyle(fontSize: 11, color: Colors.white38),
            ),
            trailing: IconButton(
              icon: const Icon(Icons.copy, size: 20),
              onPressed: () => _copyToClipboard(item.id),
              tooltip: 'Copy to clipboard',
            ),
            onTap: () => _copyToClipboard(item.id),
          ),
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
            child: Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(Icons.folder_off, size: 64, color: Colors.white24),
                  SizedBox(height: 16),
                  Text('No files yet', style: TextStyle(color: Colors.white38)),
                ],
              ),
            ),
          )
        else
          Expanded(
            child: ListView.builder(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              itemCount: files.length,
              itemBuilder: (context, index) {
                final file = files[index];
                return Card(
                  margin: const EdgeInsets.symmetric(vertical: 4),
                  child: ListTile(
                    leading: Icon(_fileIcon(file.mimeType), color: Colors.blue),
                    title: Text(file.filename),
                    subtitle: Text(
                      '${_formatSize(file.blobSize)} - ${_formatTimestamp(file.createdAt)}',
                      style: const TextStyle(
                        fontSize: 11,
                        color: Colors.white38,
                      ),
                    ),
                    trailing: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        IconButton(
                          icon: const Icon(Icons.download, size: 20),
                          onPressed: () =>
                              _downloadFile(file.id, file.filename),
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
                  ),
                );
              },
            ),
          ),
      ],
    );
  }

  IconData _fileIcon(String mimeType) {
    if (mimeType.startsWith('image/')) return Icons.image;
    if (mimeType.startsWith('video/')) return Icons.videocam;
    if (mimeType.startsWith('audio/')) return Icons.audiotrack;
    if (mimeType.startsWith('text/')) return Icons.description;
    if (mimeType.contains('pdf')) return Icons.picture_as_pdf;
    if (mimeType.contains('zip') ||
        mimeType.contains('tar') ||
        mimeType.contains('gz')) {
      return Icons.archive;
    }
    return Icons.insert_drive_file;
  }

  String _formatSize(int bytes) {
    if (bytes < 1024) return '$bytes B';
    if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
    if (bytes < 1024 * 1024 * 1024) {
      return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
    }
    return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(1)} GB';
  }

  String _formatTimestamp(String rfc3339) {
    try {
      final dt = DateTime.parse(rfc3339).toLocal();
      final now = DateTime.now();
      final diff = now.difference(dt);

      if (diff.inMinutes < 1) return 'Just now';
      if (diff.inHours < 1) return '${diff.inMinutes}m ago';
      if (diff.inDays < 1) return '${diff.inHours}h ago';
      if (diff.inDays < 7) return '${diff.inDays}d ago';
      return '${dt.month}/${dt.day}/${dt.year}';
    } catch (_) {
      return rfc3339;
    }
  }
}
