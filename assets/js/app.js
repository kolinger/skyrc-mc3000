App = function () {
};

App.prototype.bind = function () {
    var self = this;

    var adjustScrollHeight = function () {
        var height = $(window).height();
        height -= $('.top').outerHeight(true);
        $('.scroll-view').height(height + 'px');
    };
    adjustScrollHeight();
    var scrollAdjustTimeout = null;
    $(window).on('resize', function () {
        if (scrollAdjustTimeout) {
            clearTimeout(scrollAdjustTimeout);
        }
        scrollAdjustTimeout = setTimeout(adjustScrollHeight, 100);
    });

    var interval = 250;
    var loading = function () {
        var wrapper = $('.loading');
        if (wrapper.length) {
            var dots = wrapper.find('[data-loading]');
            var count = dots.text().length;
            if (count >= 3) {
                count = 0;
            } else {
                count++;
            }
            dots.text('.'.repeat(count));
            setTimeout(loading, interval);
        }
    };
    setTimeout(loading, interval);

    var ble = function () {
        $('.scan-result').text('Scanning... This can take a while...');
        self.request('get', '/scan/trigger');
    };
    $(document).on('click', '.scan button', ble);
    if ($('.scan').length) {
        ble();
    }

    $(document).on('click', '.scan-result [data-address]', function (e) {
        e.preventDefault();
        var selection = $(this);
        var url = '/scan/select?ble_address=' + encodeURIComponent(selection.attr('data-address'));
        self.request('get', url, null, function () {
            window.location.href = '/';
        });
    });
};

App.prototype.update = function (data) {
    var self = this;
    var payload = JSON.parse(data);
    if (payload.hasOwnProperty('battery_info')) {
        payload = payload.battery_info;
        var table = $('.status-table');
        var nth = payload.slot + 2;
        table.find('[data-led] td:nth-child(' + nth + ') .led').attr('class', 'led ' + payload.led);
        table.find('[data-type] td:nth-child(' + nth + ')').text(payload.type);
        table.find('[data-mode] td:nth-child(' + nth + ')').text(payload.mode);
        table.find('[data-status] td:nth-child(' + nth + ')').text(payload.status);
        table.find('[data-voltage] td:nth-child(' + nth + ')').text(payload.voltage + ' V');
        table.find('[data-current] td:nth-child(' + nth + ')').text(payload.current + ' A');
        table.find('[data-capacity] td:nth-child(' + nth + ')').text(payload.capacity + ' mAh');
        table.find('[data-time] td:nth-child(' + nth + ')').text(payload.time);
        table.find('[data-temperature] td:nth-child(' + nth + ')').text(payload.temperature + ' ??C');
        table.find('[data-resistance] td:nth-child(' + nth + ')').text(payload.resistance + ' m??');
        table.find('tr').each(function () {
            var row = $(this);
            row.find('td:nth-child(' + nth + ')').attr('class', payload.status);
        });
    } else if (payload.hasOwnProperty('scan_results')) {
        $('.scan-result').html(payload.scan_results);
    }
};

App.prototype.request = function (method, endpoint, data, callback) {
    var config = {
        method: method,
        headers: {
            'Authentication': window.token,
        },
        data: data,
        error: function (jqXHR, textStatus, errorThrown) {
            alert(textStatus + ': ' + errorThrown);
        }
    };

    if (callback) {
        config['success'] = callback;
    }

    $.ajax(endpoint, config);
};

$(function () {
    var app = new App();
    window.app = app;
    app.bind();
});
