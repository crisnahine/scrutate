% Sample MATLAB file (no Objective-C markers, so it is sniffed as MATLAB).
function out = scale_matrix(m, factor)
  % Multiply every element.
  out = m * factor;
end

function total = sum_all(m)
  total = sum(m(:));
end
