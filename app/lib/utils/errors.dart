import '../src/rust/api/clipper.dart';

String displayError(Object error) {
  if (error is BridgeError) {
    return error.message;
  }
  return error.toString();
}
