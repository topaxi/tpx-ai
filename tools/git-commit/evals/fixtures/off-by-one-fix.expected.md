# off-by-one-fix

A one-line bug fix: integer division `total / self.per_page` dropped the partial
final page; `div_ceil` rounds up so the last partial page is counted.

## Should
- Describe it as a fix to the page count / pagination rounding (off-by-one on the
  final partial page), imperative mood.
- Scope to `pagination`.

## Should NOT
- Claim new features, configuration, or behaviour beyond the rounding fix.
- Invent a changelog of unrelated pagination capabilities.
