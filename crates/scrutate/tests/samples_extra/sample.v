// A 4-bit counter with synchronous reset.
module counter (
    input wire clk,
    input wire rst,
    output reg [3:0] count
);
    always @(posedge clk) begin
        if (rst) count <= 4'b0;
        else count <= count + 1;
    end
endmodule

module top (input wire clk, output wire [3:0] q);
    counter c0 (.clk(clk), .rst(1'b0), .count(q));
endmodule
