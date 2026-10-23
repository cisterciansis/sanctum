# Proofs for LAWS.bend. `bend PROOF.bend` must print ALL PROOFS CHECK.

import Base
import ./LAWS.bend as Laws
import ./sanctum.bend as Sanctum

# Induction on xs; each cons rewrites by the induction hypothesis.
def Laws.drain_exact(xs):
  match xs:
    case Nil{}:
      {==}
    case Con{h, t}:
      %Laws.drain_exact(t) : {h <> Sanctum.drain(t) == h <> _ : List<U32>}
      {==}

def Laws.drain_length(xs):
  match xs:
    case Nil{}:
      {==}
    case Con{h, t}:
      %Laws.drain_length(t) : {1n+List.length(&1, U32, Sanctum.drain(t)) == 1n+_ : Nat}
      {==}
