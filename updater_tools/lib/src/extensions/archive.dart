import 'dart:io';
import 'dart:isolate';

import 'package:archive/archive_io.dart';
import 'package:path/path.dart' as p;

/// Functions for archiving directories.
extension DirectoryArchive on Directory {
  /// Copies this directory to a temporary directory and zips it.
  Future<File> zipToTempFile() async {
    final tempDir = await Directory.systemTemp.createTemp();
    final outFile = File(p.join(tempDir.path, '${p.basename(path)}.zip'));
    await Isolate.run(() {
      ZipFileEncoder().zipDirectory(this, filename: outFile.path);
    });
    return outFile;
  }
}

/// Functions for unarchiving files.
extension FileArchive on File {
  /// Extracts this zip file to the [outputDirectory] directory in a separate
  /// isolate.
  Future<void> extractZip({required Directory outputDirectory}) async {
    await Isolate.run(() async {
      final inputStream = InputFileStream(path);
      final archive = ZipDecoder().decodeBuffer(inputStream);
      await extractArchiveToDisk(archive, outputDirectory.path);
      inputStream.closeSync();
    });
  }
}
