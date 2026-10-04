export function createPressGuard() {
  let last = -Infinity;
  return (now: number) => {
    if (now - last < 400) return false;
    last = now;
    return true;
  };
}
