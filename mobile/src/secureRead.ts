export async function readWithDeadline(
  read: () => Promise<string | null>,
  deadlineMs = 30_000,
): Promise<string | null> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      read(),
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(
                "Fingerprint unlock did not finish. Open Clipper with the phone unlocked and retry.",
              ),
            ),
          deadlineMs,
        );
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}
