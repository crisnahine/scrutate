# Crystal sample
module Billing
  class Invoice
    # Total of lines.
    def total
      lines.sum
    end
  end
  struct Pt
    property x : Int32
  end
  macro twice(x)
    {{x}} * 2
  end
end
def helper(x)
  puts x
end
