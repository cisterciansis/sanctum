# Protocol model: a mailbox is the ULID-ordered list of deposited envelopes
# (modeled as U32 ids). `drain` is what the client does on poll: walk the
# listing in order and deliver each message.

import Base

def drain(xs: List<U32>) -> List<U32>:
  match xs:
    case Nil{}:
      Nil{}
    case Con{h, t}:
      h <> drain(t)
