/**
 * Returns a shuffled copy of the list using Fisher–Yates.
 *
 * `sort(() => Math.random() - 0.5)` is the tempting one-liner and it is not a
 * shuffle — comparison sorts assume a consistent comparator, so the result is
 * measurably biased toward the original order.
 */
export function shuffled<T>(items: readonly T[]): T[] {
  const copy = [...items]
  for (let i = copy.length - 1; i > 0; i -= 1) {
    const j = Math.floor(Math.random() * (i + 1))
    ;[copy[i], copy[j]] = [copy[j], copy[i]]
  }
  return copy
}
