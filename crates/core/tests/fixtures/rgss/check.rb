# Loads a Marshal file written by Yakusu and prints the event lists as
# "code|indent|params-json" lines, plus whether shared objects stayed shared.
require "json"
load File.join(__dir__, "make_classes.rb")
obj = Marshal.load(File.binread(ARGV[0]))
def lists(o, out)
  case o
  when Array then o.each { |x| lists(x, out) }
  when Hash then o.each_value { |x| lists(x, out) }
  else
    if o.instance_variable_defined?(:@list)
      out << o.instance_variable_get(:@list)
    end
    %i[@events @pages].each { |iv| lists(o.instance_variable_get(iv), out) if o.instance_variable_defined?(iv) }
  end
end
out = []
lists(obj, out)
out.each do |l|
  l.each { |c| puts [c.instance_variable_get(:@code), c.instance_variable_get(:@indent), c.instance_variable_get(:@parameters).to_json].join("|") }
  cmts = l.select { |c| [108, 408].include?(c.instance_variable_get(:@code)) }
  puts "shared=#{cmts[0].instance_variable_get(:@parameters)[0].equal?(cmts[1].instance_variable_get(:@parameters)[0])}" if cmts.size == 2
end
