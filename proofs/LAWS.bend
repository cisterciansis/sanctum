# Sanctum protocol laws — human-written, machine-checked (`bend PROOF.bend`).
# Each law is a precise claim about the protocol model in sanctum.bend.
# The implementor writes PROOF.bend; it may not touch this file.

import Base
import ./sanctum.bend as Sanctum

# Draining a mailbox delivers every deposited message, unchanged and in
# deposit order: delivery loses nothing, duplicates nothing, reorders nothing.
law drain_exact:
  for xs: List<U32>
  {Sanctum.drain(xs) == xs : List<U32>}

# Delivery count equals deposit count (no silent drops).
law drain_length:
  for xs: List<U32>
  {List.length(&1, U32, Sanctum.drain(xs)) == List.length(&1, U32, xs) : Nat}
