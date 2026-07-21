# Shared lessons

- Private workspace readers must call the central private-state resolver before
  lower-level opens. This keeps safe legacy migration, unsafe-layout errors,
  generated ignore rules, and ordinary absence consistent across every entry
  point.
